#![cfg(test)]

pub mod messages {
    use crate::SnowflakePosition;
    use crate::messages::{BatchType, Message};
    use crate::utils::get_next_message_position_sync;

    /// Create a simple two-message batch: user then assistant.
    /// Returns (user_msg, assistant_msg, batch_id).
    pub fn simple_user_assistant_batch(
        user_text: impl Into<String>,
        assistant_text: impl Into<String>,
    ) -> (Message, Message, SnowflakePosition) {
        let batch_id = get_next_message_position_sync();
        let user = Message::user_in_batch(batch_id, 0, user_text.into());
        let mut assistant = Message::assistant_in_batch(batch_id, 1, assistant_text.into());
        if assistant.batch_type.is_none() {
            assistant.batch_type = Some(BatchType::UserRequest);
        }
        (user, assistant, batch_id)
    }
}

pub mod memory {
    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::Value as JsonValue;

    use crate::memory::{
        ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult, MemorySearchResult,
        MemoryStore, SearchOptions, SharedBlockInfo, StructuredDocument,
    };

    /// Configurable mock MemoryStore for testing different block configurations.
    ///
    /// Returns Core and Working blocks with mock content. Use builder methods
    /// to configure specific behaviors.
    #[derive(Debug, Default)]
    pub struct MockMemoryStore {
        /// If true, Working blocks are pinned (default behavior).
        /// If false, returns a mix of pinned and unpinned Working blocks.
        pub working_blocks_pinned: bool,
    }

    impl MockMemoryStore {
        /// Create a new MockMemoryStore with all Working blocks pinned (default).
        pub fn new() -> Self {
            Self {
                working_blocks_pinned: true,
            }
        }

        /// Create a MockMemoryStore with unpinned Working blocks for testing batch_block_ids.
        pub fn with_unpinned_working_blocks() -> Self {
            Self {
                working_blocks_pinned: false,
            }
        }
    }

    #[async_trait]
    impl MemoryStore for MockMemoryStore {
        async fn create_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _description: &str,
            _block_type: BlockType,
            schema: BlockSchema,
            _char_limit: usize,
        ) -> MemoryResult<StructuredDocument> {
            Ok(StructuredDocument::new(schema))
        }

        async fn get_block(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        async fn get_block_metadata(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            Ok(None)
        }

        async fn list_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
            Ok(Vec::new())
        }

        async fn list_blocks_by_type(
            &self,
            _agent_id: &str,
            block_type: BlockType,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            // Return mock blocks based on type.
            match block_type {
                BlockType::Core => Ok(vec![BlockMetadata {
                    id: "core-1".to_string(),
                    agent_id: "test-agent".to_string(),
                    label: "core_memory".to_string(),
                    description: "Core agent memory".to_string(),
                    block_type: BlockType::Core,
                    schema: BlockSchema::text(),
                    char_limit: 1000,
                    permission: pattern_db::models::MemoryPermission::ReadWrite,
                    pinned: true,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]),
                BlockType::Working => {
                    if self.working_blocks_pinned {
                        // Default: single pinned Working block.
                        Ok(vec![BlockMetadata {
                            id: "working-1".to_string(),
                            agent_id: "test-agent".to_string(),
                            label: "working_memory".to_string(),
                            description: "Working context".to_string(),
                            block_type: BlockType::Working,
                            schema: BlockSchema::text(),
                            char_limit: 2000,
                            permission: pattern_db::models::MemoryPermission::ReadWrite,
                            pinned: true,
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        }])
                    } else {
                        // Unpinned mode: mix of pinned and unpinned blocks for testing filtering.
                        Ok(vec![
                            // Unpinned block - should be excluded by default.
                            BlockMetadata {
                                id: "ephemeral-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "ephemeral_context".to_string(),
                                description: "Ephemeral context block".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: pattern_db::models::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                            // Another unpinned block.
                            BlockMetadata {
                                id: "ephemeral-2".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "user_profile".to_string(),
                                description: "User profile block".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: pattern_db::models::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                            // Pinned block - should always be included.
                            BlockMetadata {
                                id: "pinned-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "pinned_config".to_string(),
                                description: "Pinned configuration".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: pattern_db::models::MemoryPermission::ReadWrite,
                                pinned: true,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                        ])
                    }
                }
                _ => Ok(Vec::new()),
            }
        }

        async fn list_all_blocks_by_label_prefix(
            &self,
            _prefix: &str,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            Ok(Vec::new())
        }

        async fn delete_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn get_rendered_content(
            &self,
            _agent_id: &str,
            label: &str,
        ) -> MemoryResult<Option<String>> {
            // Return mock content based on label.
            Ok(Some(format!("Content for {}", label)))
        }

        async fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn mark_dirty(&self, _agent_id: &str, _label: &str) {}

        async fn insert_archival(
            &self,
            _agent_id: &str,
            _content: &str,
            _metadata: Option<JsonValue>,
        ) -> MemoryResult<String> {
            Ok("test-archival-id".to_string())
        }

        async fn search_archival(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            Ok(Vec::new())
        }

        async fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn search(
            &self,
            _agent_id: &str,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn search_all(
            &self,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn list_shared_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
            Ok(Vec::new())
        }

        async fn get_shared_block(
            &self,
            _requester_agent_id: &str,
            _owner_agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        async fn set_block_pinned(
            &self,
            _agent_id: &str,
            _label: &str,
            _pinned: bool,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn set_block_type(
            &self,
            _agent_id: &str,
            _label: &str,
            _block_type: BlockType,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn update_block_schema(
            &self,
            _agent_id: &str,
            _label: &str,
            _schema: BlockSchema,
        ) -> MemoryResult<()> {
            Ok(())
        }
    }
}
