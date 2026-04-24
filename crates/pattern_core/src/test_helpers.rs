#![cfg(test)]

// The pre-v3 `messages` helper module depended on the now-staged legacy
// `messages/` module and on `SnowflakePosition` (superseded by jiff
// `Timestamp`). The helpers are retired; if message-batch helpers are needed
// again, rebuild them on top of `types::batch::MessageBatch`.

pub mod memory {
    use chrono::Utc;
    use serde_json::Value as JsonValue;

    use crate::memory::StructuredDocument;
    use crate::traits::MemoryStore;
    use crate::types::block::BlockCreate;
    use crate::types::memory_types::{
        ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema, BlockType,
        MemoryResult, MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo,
        UndoRedoDepth, UndoRedoOp,
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

    impl MemoryStore for MockMemoryStore {
        fn create_block(
            &self,
            _agent_id: &str,
            create: BlockCreate,
        ) -> MemoryResult<StructuredDocument> {
            Ok(StructuredDocument::new(create.schema))
        }

        fn get_block(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        fn get_block_metadata(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            Ok(None)
        }

        fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
            // Return mock blocks based on type filter if present.
            match filter.block_type {
                Some(BlockType::Core) => Ok(vec![BlockMetadata {
                    id: "core-1".to_string(),
                    agent_id: "test-agent".to_string(),
                    label: "core_memory".to_string(),
                    description: "Core agent memory".to_string(),
                    block_type: BlockType::Core,
                    schema: BlockSchema::text(),
                    char_limit: 1000,
                    permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                    pinned: true,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]),
                Some(BlockType::Working) => {
                    if self.working_blocks_pinned {
                        Ok(vec![BlockMetadata {
                            id: "working-1".to_string(),
                            agent_id: "test-agent".to_string(),
                            label: "working_memory".to_string(),
                            description: "Working context".to_string(),
                            block_type: BlockType::Working,
                            schema: BlockSchema::text(),
                            char_limit: 2000,
                            permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                            pinned: true,
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        }])
                    } else {
                        Ok(vec![
                            BlockMetadata {
                                id: "ephemeral-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "ephemeral_context".to_string(),
                                description: "Ephemeral context block".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                            BlockMetadata {
                                id: "ephemeral-2".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "user_profile".to_string(),
                                description: "User profile block".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                            BlockMetadata {
                                id: "pinned-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "pinned_config".to_string(),
                                description: "Pinned configuration".to_string(),
                                block_type: BlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: true,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            },
                        ])
                    }
                }
                None => Ok(Vec::new()),
            }
        }

        fn delete_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn get_rendered_content(
            &self,
            _agent_id: &str,
            label: &str,
        ) -> MemoryResult<Option<String>> {
            Ok(Some(format!("Content for {}", label)))
        }

        fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn mark_dirty(&self, _agent_id: &str, _label: &str) {}

        fn insert_archival(
            &self,
            _agent_id: &str,
            _content: &str,
            _metadata: Option<JsonValue>,
        ) -> MemoryResult<String> {
            Ok("test-archival-id".to_string())
        }

        fn search_archival(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            Ok(Vec::new())
        }

        fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn search(
            &self,
            _query: &str,
            _options: SearchOptions,
            _scope: MemorySearchScope,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        fn list_shared_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
            Ok(Vec::new())
        }

        fn get_shared_block(
            &self,
            _requester_agent_id: &str,
            _owner_agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        fn update_block_metadata(
            &self,
            _agent_id: &str,
            _label: &str,
            _patch: BlockMetadataPatch,
        ) -> MemoryResult<()> {
            Ok(())
        }

        fn undo_redo(&self, _agent_id: &str, _label: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
            Ok(false)
        }

        fn history_depth(&self, _agent_id: &str, _label: &str) -> MemoryResult<UndoRedoDepth> {
            Ok(UndoRedoDepth { undo: 0, redo: 0 })
        }
    }
}
