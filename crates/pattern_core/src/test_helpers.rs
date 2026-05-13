#![cfg(test)]

// The pre-v3 `messages` helper module depended on the now-staged legacy
// `messages/` module and on `SnowflakePosition` (superseded by jiff
// `Timestamp`). The helpers are retired; if message-batch helpers are needed
// again, rebuild them on top of `types::batch::MessageBatch`.

pub mod memory {
    use jiff::Timestamp;
    use serde_json::Value as JsonValue;

    use crate::memory::StructuredDocument;
    use crate::traits::MemoryStore;
    use crate::types::block::BlockCreate;
    use crate::types::memory_types::{
        ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema,
        MemoryBlockType, MemoryResult, MemorySearchResult, MemorySearchScope, Scope,
        SearchOptions, SharedBlockInfo, UndoRedoDepth, UndoRedoOp,
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
        fn commit_write(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> { Ok(()) }
        fn create_or_replace_block(&self, scope: &Scope, create: BlockCreate) -> MemoryResult<StructuredDocument> { self.create_block(scope, create) }
        fn create_block(
            &self,
            _scope: &Scope,
            create: BlockCreate,
        ) -> MemoryResult<StructuredDocument> {
            Ok(StructuredDocument::new(create.schema))
        }

        fn get_block(
            &self,
            _scope: &Scope,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        fn get_block_metadata(
            &self,
            _scope: &Scope,
            _label: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            Ok(None)
        }

        fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
            // Return mock blocks based on type filter if present.
            match filter.block_type {
                Some(MemoryBlockType::Core) => Ok(vec![BlockMetadata {
                    id: "core-1".to_string(),
                    agent_id: "test-agent".to_string(),
                    label: "core_memory".to_string(),
                    description: "Core agent memory".to_string(),
                    block_type: MemoryBlockType::Core,
                    schema: BlockSchema::text(),
                    char_limit: 1000,
                    permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                    pinned: true,
                    created_at: Timestamp::now(),
                    updated_at: Timestamp::now(),
                }]),
                Some(MemoryBlockType::Working) => {
                    if self.working_blocks_pinned {
                        Ok(vec![BlockMetadata {
                            id: "working-1".to_string(),
                            agent_id: "test-agent".to_string(),
                            label: "working_memory".to_string(),
                            description: "Working context".to_string(),
                            block_type: MemoryBlockType::Working,
                            schema: BlockSchema::text(),
                            char_limit: 2000,
                            permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                            pinned: true,
                            created_at: Timestamp::now(),
                            updated_at: Timestamp::now(),
                        }])
                    } else {
                        Ok(vec![
                            BlockMetadata {
                                id: "ephemeral-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "ephemeral_context".to_string(),
                                description: "Ephemeral context block".to_string(),
                                block_type: MemoryBlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Timestamp::now(),
                                updated_at: Timestamp::now(),
                            },
                            BlockMetadata {
                                id: "ephemeral-2".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "user_profile".to_string(),
                                description: "User profile block".to_string(),
                                block_type: MemoryBlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: false,
                                created_at: Timestamp::now(),
                                updated_at: Timestamp::now(),
                            },
                            BlockMetadata {
                                id: "pinned-1".to_string(),
                                agent_id: "test-agent".to_string(),
                                label: "pinned_config".to_string(),
                                description: "Pinned configuration".to_string(),
                                block_type: MemoryBlockType::Working,
                                schema: BlockSchema::text(),
                                char_limit: 2000,
                                permission: crate::types::memory_types::MemoryPermission::ReadWrite,
                                pinned: true,
                                created_at: Timestamp::now(),
                                updated_at: Timestamp::now(),
                            },
                        ])
                    }
                }
                None => Ok(Vec::new()),
            }
        }

        fn delete_block(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn get_rendered_content(
            &self,
            _scope: &Scope,
            label: &str,
        ) -> MemoryResult<Option<String>> {
            Ok(Some(format!("Content for {}", label)))
        }

        fn persist_block(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn mark_dirty(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn insert_archival(
            &self,
            _scope: &Scope,
            _content: &str,
            _metadata: Option<JsonValue>,
        ) -> MemoryResult<String> {
            Ok("test-archival-id".to_string())
        }

        fn search_archival(
            &self,
            _scope: &Scope,
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

        fn list_shared_blocks(&self, _scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
            Ok(Vec::new())
        }

        fn get_shared_block(
            &self,
            _requester: &Scope,
            _owner: &Scope,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        fn update_block_metadata(
            &self,
            _scope: &Scope,
            _label: &str,
            _patch: BlockMetadataPatch,
        ) -> MemoryResult<()> {
            Ok(())
        }

        fn undo_redo(&self, _scope: &Scope, _label: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
            Ok(false)
        }

        fn history_depth(&self, _scope: &Scope, _label: &str) -> MemoryResult<UndoRedoDepth> {
            Ok(UndoRedoDepth { undo: 0, redo: 0 })
        }
    }
}
