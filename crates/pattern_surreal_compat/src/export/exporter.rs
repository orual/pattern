//! Agent exporter implementation

use chrono::Utc;
use cid::Cid;
use iroh_car::{CarHeader, CarWriter};
use multihash_codetable::Code;
use multihash_codetable::MultihashDigest;
use serde_ipld_dagcbor::to_vec as encode_dag_cbor;
use surrealdb::Surreal;
use tokio::io::AsyncWrite;

use crate::{
    AgentId, CoreError,
    agent_entity::AgentRecord,
    db::entity::DbEntity,
    export::{
        DEFAULT_CHUNK_SIZE, DEFAULT_MEMORY_CHUNK_SIZE, EXPORT_VERSION, MAX_BLOCK_BYTES,
        types::{
            AgentExport, AgentRecordExport, ConstellationExport, ExportManifest, ExportStats,
            ExportType, GroupExport, MemoryChunk, MessageChunk,
        },
    },
    groups::{AgentGroup, Constellation, GroupMembership},
    id::{ConstellationId, GroupId},
    message::Message,
};

type Result<T> = std::result::Result<T, CoreError>;

/// Options for exporting an agent
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Whether to include message history
    pub include_messages: bool,

    /// Maximum messages per chunk
    pub chunk_size: usize,

    /// Optional time filter for messages
    pub messages_since: Option<chrono::DateTime<chrono::Utc>>,

    /// Whether to compress the output
    pub compress: bool,

    /// Whether to exclude embeddings from export (reduces file size significantly)
    pub exclude_embeddings: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_messages: true,
            chunk_size: DEFAULT_CHUNK_SIZE,
            messages_since: None,
            compress: false,
            exclude_embeddings: false,
        }
    }
}

/// Agent exporter
pub struct AgentExporter<C>
where
    C: surrealdb::Connection + Clone,
{
    db: Surreal<C>,
}

impl<C> AgentExporter<C>
where
    C: surrealdb::Connection + Clone,
{
    /// Create a new exporter
    pub fn new(db: Surreal<C>) -> Self {
        Self { db }
    }

    /// Strip embeddings from memory blocks if requested
    fn maybe_strip_memory_embeddings(
        &self,
        memories: &[(
            crate::memory::MemoryBlock,
            crate::agent_entity::AgentMemoryRelation,
        )],
        options: &ExportOptions,
    ) -> Vec<(
        crate::memory::MemoryBlock,
        crate::agent_entity::AgentMemoryRelation,
    )> {
        if options.exclude_embeddings {
            memories
                .iter()
                .map(|(mem, rel)| {
                    let mut mem_copy = mem.clone();
                    mem_copy.embedding = None;
                    mem_copy.embedding_model = None;
                    (mem_copy, rel.clone())
                })
                .collect()
        } else {
            memories.to_vec()
        }
    }

    /// Strip embeddings from messages if requested
    fn maybe_strip_message_embeddings(
        &self,
        messages: &[(
            crate::message::Message,
            crate::message::AgentMessageRelation,
        )],
        options: &ExportOptions,
    ) -> Vec<(
        crate::message::Message,
        crate::message::AgentMessageRelation,
    )> {
        if options.exclude_embeddings {
            messages
                .iter()
                .map(|(msg, rel)| {
                    let mut msg_copy = msg.clone();
                    msg_copy.embedding = None;
                    msg_copy.embedding_model = None;
                    (msg_copy, rel.clone())
                })
                .collect()
        } else {
            messages.to_vec()
        }
    }

    /// Helper to create a CID from serialized data
    fn create_cid(data: &[u8]) -> Result<Cid> {
        // Use Blake3-256 hash and DAG-CBOR codec
        const DAG_CBOR_CODEC: u64 = 0x71;
        let hash = Code::Blake3_256.digest(data);
        Ok(Cid::new_v1(DAG_CBOR_CODEC, hash))
    }

    /// Export an agent to a CAR file
    pub async fn export_to_car(
        &self,
        agent_id: AgentId,
        mut output: impl AsyncWrite + Unpin + Send,
        options: ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();

        // Load the agent record
        let mut agent = AgentRecord::load_with_relations(&self.db, &agent_id)
            .await
            .map_err(|e| {
                CoreError::from(e).with_db_context(
                    format!("SELECT * FROM agent WHERE id = '{}'", agent_id),
                    "agent",
                )
            })?
            .ok_or_else(|| CoreError::AgentGroupError {
                group_name: "export".to_string(),
                operation: "load_agent".to_string(),
                cause: format!("Agent '{}' not found", agent_id),
            })?;

        // Load message history and memory blocks (like CLI does)
        let (messages_result, memories_result) = tokio::join!(
            agent.load_message_history(&self.db, true),
            crate::db::ops::get_agent_memories(&self.db, &agent.id)
        );

        // Handle results
        if let Ok(messages) = messages_result {
            tracing::info!(
                "Loaded {} messages for agent {}",
                messages.len(),
                agent.name
            );
            agent.messages = messages;
        }

        if let Ok(memory_tuples) = memories_result {
            tracing::info!(
                "Loaded {} memory blocks for agent {}",
                memory_tuples.len(),
                agent.name
            );
            agent.memories = memory_tuples
                .into_iter()
                .map(|(memory_block, access_level)| {
                    use crate::id::RelationId;
                    let relation = crate::agent_entity::AgentMemoryRelation {
                        id: RelationId::nil(),
                        in_id: agent.id.clone(),
                        out_id: memory_block.id.clone(),
                        access_level,
                        created_at: chrono::Utc::now(),
                    };
                    (memory_block, relation)
                })
                .collect();
        }

        // First export the agent and collect all blocks
        let (agent_export, agent_blocks, mut stats) =
            self.export_agent_to_blocks(&agent, &options).await?;

        // Create the agent export data
        let agent_export_data =
            encode_dag_cbor(&agent_export).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "AgentExport".to_string(),
                cause: e,
            })?;
        if agent_export_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding AgentExport".to_string(),
                cause: iroh_car::Error::Parsing("agent export block too large".to_string()),
            });
        }
        let agent_export_cid = Self::create_cid(&agent_export_data)?;

        // Update stats
        stats.total_blocks += 1; // For the AgentExport itself

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Agent,
            stats,
            data_cid: agent_export_cid,
        };

        // Serialize manifest
        let manifest_data =
            encode_dag_cbor(&manifest).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "ExportManifest".to_string(),
                cause: e,
            })?;
        if manifest_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding ExportManifest".to_string(),
                cause: iroh_car::Error::Parsing("manifest block too large".to_string()),
            });
        }
        let manifest_cid = Self::create_cid(&manifest_data)?;

        // Create CAR writer with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut car_writer = CarWriter::new(header, &mut output);

        // Write manifest first
        car_writer
            .write(manifest_cid, &manifest_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing manifest to CAR".to_string(),
                cause: e,
            })?;

        // Write agent export
        car_writer
            .write(agent_export_cid, &agent_export_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing agent export to CAR".to_string(),
                cause: e,
            })?;

        // Write all the agent blocks (agent record, memories, messages)
        for (cid, data) in agent_blocks {
            car_writer
                .write(cid, &data)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "writing agent block to CAR".to_string(),
                    cause: e,
                })?;
        }

        // Flush the writer
        car_writer.finish().await.map_err(|e| CoreError::CarError {
            operation: "finishing CAR write".to_string(),
            cause: e,
        })?;

        Ok(manifest)
    }

    /// Export an agent to blocks without writing to CAR file
    pub(crate) async fn export_agent_to_blocks(
        &self,
        agent: &AgentRecord,
        options: &ExportOptions,
    ) -> Result<(AgentExport, Vec<(Cid, Vec<u8>)>, ExportStats)> {
        let mut blocks = Vec::new();
        let mut stats = ExportStats {
            memory_count: 0,
            message_count: 0,
            chunk_count: 0,
            total_blocks: 0,
            uncompressed_size: 0,
            compressed_size: None,
        };

        let mut memory_chunk_cids = Vec::new();
        let mut message_chunk_cids = Vec::new();

        // Helper to write a block with size enforcement
        let mut write_block = |cid: Cid, data: Vec<u8>| -> Result<()> {
            if data.len() > MAX_BLOCK_BYTES {
                return Err(CoreError::CarError {
                    operation: format!("block exceeds {} bytes", MAX_BLOCK_BYTES),
                    cause: iroh_car::Error::Parsing("block too large".to_string()),
                });
            }
            stats.total_blocks += 1;
            stats.uncompressed_size += data.len() as u64;
            blocks.push((cid, data));
            Ok(())
        };

        // Export memories in chunks (two-phase to wire next_chunk)
        if !agent.memories.is_empty() {
            stats.memory_count = agent.memories.len() as u64;

            let mut current: Vec<(
                crate::memory::MemoryBlock,
                crate::agent_entity::AgentMemoryRelation,
            )> = Vec::new();
            let mut pending_chunks: Vec<
                Vec<(
                    crate::memory::MemoryBlock,
                    crate::agent_entity::AgentMemoryRelation,
                )>,
            > = Vec::new();

            let encode_mem_probe = |chunk_id: u32,
                                    items: &Vec<(
                crate::memory::MemoryBlock,
                crate::agent_entity::AgentMemoryRelation,
            )>|
             -> Result<usize> {
                // Strip embeddings if requested to get accurate size estimate
                let processed_items = if options.exclude_embeddings {
                    items
                        .iter()
                        .map(|(mem, rel)| {
                            let mut mem_copy = mem.clone();
                            mem_copy.embedding = None;
                            mem_copy.embedding_model = None;
                            (mem_copy, rel.clone())
                        })
                        .collect()
                } else {
                    items.clone()
                };

                let chunk = MemoryChunk {
                    chunk_id,
                    memories: processed_items,
                    next_chunk: None,
                };
                let data =
                    encode_dag_cbor(&chunk).map_err(|e| CoreError::DagCborEncodingError {
                        data_type: "MemoryChunk".to_string(),
                        cause: e,
                    })?;
                Ok(data.len())
            };

            let mut chunk_id: u32 = 0;
            for item in agent.memories.iter().cloned() {
                let mut test_vec = current.clone();
                test_vec.push(item.clone());
                let est = encode_mem_probe(chunk_id, &test_vec)?;
                if est <= (MAX_BLOCK_BYTES.saturating_sub(64))
                    && test_vec.len() <= DEFAULT_MEMORY_CHUNK_SIZE
                {
                    current = test_vec;
                } else {
                    if !current.is_empty() {
                        pending_chunks.push(current);
                        stats.chunk_count += 1;
                        chunk_id += 1;
                        current = Vec::new();
                    }
                    // Ensure single item fits
                    let est_single = encode_mem_probe(chunk_id, &vec![item.clone()])?;
                    if est_single > MAX_BLOCK_BYTES {
                        return Err(CoreError::CarError {
                            operation: "encoding MemoryChunk".to_string(),
                            cause: iroh_car::Error::Parsing(
                                "single memory item exceeds block limit".to_string(),
                            ),
                        });
                    }
                    current.push(item);
                }
            }
            if !current.is_empty() {
                pending_chunks.push(current);
                stats.chunk_count += 1;
            }

            // Finalize chunks in reverse to set next_chunk
            let mut next: Option<Cid> = None;
            let mut finalized_cids_rev: Vec<Cid> = Vec::new();
            let mut cid_chunk_id = (pending_chunks.len() as u32).saturating_sub(1);
            for items in pending_chunks.iter().rev() {
                let processed_items = self.maybe_strip_memory_embeddings(items, options);
                let chunk = MemoryChunk {
                    chunk_id: cid_chunk_id,
                    memories: processed_items,
                    next_chunk: next,
                };
                let data =
                    encode_dag_cbor(&chunk).map_err(|e| CoreError::DagCborEncodingError {
                        data_type: "MemoryChunk".to_string(),
                        cause: e,
                    })?;
                if data.len() > MAX_BLOCK_BYTES {
                    // safety check
                    return Err(CoreError::CarError {
                        operation: "finalizing MemoryChunk".to_string(),
                        cause: iroh_car::Error::Parsing(
                            "memory chunk exceeded block limit when linking".to_string(),
                        ),
                    });
                }
                let cid = Self::create_cid(&data)?;
                write_block(cid, data)?;
                finalized_cids_rev.push(cid);
                next = Some(cid);
                if cid_chunk_id > 0 {
                    cid_chunk_id -= 1;
                }
            }
            // reverse to forward order
            memory_chunk_cids = finalized_cids_rev.into_iter().rev().collect();
        }

        // Export messages in chunks (two-phase to wire next_chunk)
        if options.include_messages {
            let source: Vec<_> = if let Some(since) = options.messages_since {
                agent
                    .messages
                    .iter()
                    .filter(|(msg, _)| msg.created_at >= since)
                    .cloned()
                    .collect()
            } else {
                agent.messages.clone()
            };

            if !source.is_empty() {
                let mut current: Vec<(Message, crate::message::AgentMessageRelation)> = Vec::new();
                let mut pending_chunks: Vec<Vec<(Message, crate::message::AgentMessageRelation)>> =
                    Vec::new();

                let encode_msg_probe =
                    |chunk_id: u32,
                     items: &Vec<(Message, crate::message::AgentMessageRelation)>|
                     -> Result<usize> {
                        // Strip embeddings if requested to get accurate size estimate
                        let processed_items = if options.exclude_embeddings {
                            items
                                .iter()
                                .map(|(msg, rel)| {
                                    let mut msg_copy = msg.clone();
                                    msg_copy.embedding = None;
                                    msg_copy.embedding_model = None;
                                    (msg_copy, rel.clone())
                                })
                                .collect()
                        } else {
                            items.clone()
                        };

                        let chunk = MessageChunk {
                            chunk_id,
                            start_position: items
                                .first()
                                .and_then(|(_, rel)| rel.position.as_ref())
                                .map(|p| p.to_string())
                                .unwrap_or_default(),
                            end_position: items
                                .last()
                                .and_then(|(_, rel)| rel.position.as_ref())
                                .map(|p| p.to_string())
                                .unwrap_or_default(),
                            messages: processed_items,
                            next_chunk: None,
                        };
                        let data = encode_dag_cbor(&chunk).map_err(|e| {
                            CoreError::DagCborEncodingError {
                                data_type: "MessageChunk".to_string(),
                                cause: e,
                            }
                        })?;
                        Ok(data.len())
                    };

                let mut chunk_id: u32 = 0;
                for item in source.into_iter() {
                    let mut test_vec = current.clone();
                    test_vec.push(item.clone());
                    let est = encode_msg_probe(chunk_id, &test_vec)?;
                    if est <= (MAX_BLOCK_BYTES.saturating_sub(64))
                        && test_vec.len() <= options.chunk_size
                    {
                        current = test_vec;
                    } else {
                        if !current.is_empty() {
                            pending_chunks.push(current);
                            stats.chunk_count += 1;
                            chunk_id += 1;
                            current = Vec::new();
                        }
                        let est_single = encode_msg_probe(chunk_id, &vec![item.clone()])?;
                        if est_single > MAX_BLOCK_BYTES {
                            return Err(CoreError::CarError {
                                operation: "encoding MessageChunk".to_string(),
                                cause: iroh_car::Error::Parsing(
                                    "single message exceeds block limit".to_string(),
                                ),
                            });
                        }
                        current.push(item);
                    }
                }
                if !current.is_empty() {
                    pending_chunks.push(current);
                    stats.chunk_count += 1;
                }

                // Finalize chunks in reverse to set next_chunk
                let mut next: Option<Cid> = None;
                let mut finalized_cids_rev: Vec<Cid> = Vec::new();
                let mut cid_chunk_id = (pending_chunks.len() as u32).saturating_sub(1);
                for items in pending_chunks.iter().rev() {
                    let chunk = MessageChunk {
                        chunk_id: cid_chunk_id,
                        start_position: items
                            .first()
                            .and_then(|(_, rel)| rel.position.as_ref())
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        end_position: items
                            .last()
                            .and_then(|(_, rel)| rel.position.as_ref())
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        messages: self.maybe_strip_message_embeddings(items, options),
                        next_chunk: next,
                    };
                    let data =
                        encode_dag_cbor(&chunk).map_err(|e| CoreError::DagCborEncodingError {
                            data_type: "MessageChunk".to_string(),
                            cause: e,
                        })?;
                    if data.len() > MAX_BLOCK_BYTES {
                        return Err(CoreError::CarError {
                            operation: "finalizing MessageChunk".to_string(),
                            cause: iroh_car::Error::Parsing(
                                "message chunk exceeded block limit when linking".to_string(),
                            ),
                        });
                    }
                    let cid = Self::create_cid(&data)?;
                    write_block(cid, data)?;
                    finalized_cids_rev.push(cid);
                    next = Some(cid);
                    if cid_chunk_id > 0 {
                        cid_chunk_id -= 1;
                    }
                    stats.message_count += items.len() as u64;
                }
                message_chunk_cids = finalized_cids_rev.into_iter().rev().collect();
            }
        }

        // Build the slim export record as its own block
        let agent_export_record = AgentRecordExport::from_agent(
            agent,
            message_chunk_cids.clone(),
            memory_chunk_cids.clone(),
        );
        let agent_export_record_data =
            encode_dag_cbor(&agent_export_record).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "AgentRecordExport".to_string(),
                cause: e,
            })?;
        if agent_export_record_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding AgentRecordExport".to_string(),
                cause: iroh_car::Error::Parsing("agent metadata block too large".to_string()),
            });
        }
        let agent_export_cid = Self::create_cid(&agent_export_record_data)?;
        write_block(agent_export_cid, agent_export_record_data)?;

        let agent_export = AgentExport {
            agent_cid: agent_export_cid,
            message_chunk_cids,
            memory_chunk_cids,
        };

        Ok((agent_export, blocks, stats))
    }

    /// Export a group with all its member agents to a CAR file
    pub async fn export_group_to_car(
        &self,
        group_id: GroupId,
        mut output: impl AsyncWrite + Unpin + Send,
        options: ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();
        let mut total_stats = ExportStats {
            memory_count: 0,
            message_count: 0,
            chunk_count: 0,
            total_blocks: 0,
            uncompressed_size: 0,
            compressed_size: None,
        };

        // Load the group with all members
        let group = self.load_group_with_members(&group_id).await?;

        // Export all member agents first
        let mut agent_export_cids = Vec::new();
        let mut all_blocks = Vec::new();

        for (agent, _membership) in &group.members {
            let (agent_export, agent_blocks, stats) =
                self.export_agent_to_blocks(agent, &options).await?;

            // Serialize the agent export and get its CID
            let agent_export_data =
                encode_dag_cbor(&agent_export).map_err(|e| CoreError::DagCborEncodingError {
                    data_type: "AgentExport".to_string(),
                    cause: e,
                })?;
            if agent_export_data.len() > MAX_BLOCK_BYTES {
                return Err(CoreError::CarError {
                    operation: "encoding AgentExport".to_string(),
                    cause: iroh_car::Error::Parsing("agent export block too large".to_string()),
                });
            }
            if agent_export_data.len() > MAX_BLOCK_BYTES {
                return Err(CoreError::CarError {
                    operation: "encoding AgentExport".to_string(),
                    cause: iroh_car::Error::Parsing("agent export block too large".to_string()),
                });
            }
            let agent_export_cid = Self::create_cid(&agent_export_data)?;

            agent_export_cids.push((agent.id.clone(), agent_export_cid));
            all_blocks.push((agent_export_cid, agent_export_data));
            all_blocks.extend(agent_blocks);

            // Accumulate stats
            total_stats.memory_count += stats.memory_count;
            total_stats.message_count += stats.message_count;
            total_stats.chunk_count += stats.chunk_count;
            total_stats.total_blocks += stats.total_blocks;
            total_stats.uncompressed_size += stats.uncompressed_size;
        }

        // Create the group export
        let group_export = self.export_group(&group, &agent_export_cids).await?;

        // Serialize group export
        let group_data =
            encode_dag_cbor(&group_export).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "GroupExport".to_string(),
                cause: e,
            })?;
        if group_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding GroupExport".to_string(),
                cause: iroh_car::Error::Parsing("group export block too large".to_string()),
            });
        }
        let group_cid = Self::create_cid(&group_data)?;

        total_stats.total_blocks += 1; // For the group export itself

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Group,
            stats: total_stats,
            data_cid: group_cid,
        };

        // Serialize manifest
        let manifest_data =
            encode_dag_cbor(&manifest).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "ExportManifest".to_string(),
                cause: e,
            })?;
        if manifest_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding ExportManifest".to_string(),
                cause: iroh_car::Error::Parsing("manifest block too large".to_string()),
            });
        }
        let manifest_cid = Self::create_cid(&manifest_data)?;

        // Create CAR file with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut car_writer = CarWriter::new(header, &mut output);

        // Write manifest first
        car_writer
            .write(manifest_cid, &manifest_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing manifest to CAR".to_string(),
                cause: e,
            })?;

        // Write group block
        car_writer
            .write(group_cid, &group_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing group to CAR".to_string(),
                cause: e,
            })?;

        // Write all agent blocks
        for (cid, data) in all_blocks {
            car_writer
                .write(cid, &data)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "writing agent block to CAR".to_string(),
                    cause: e,
                })?;
        }

        car_writer.finish().await.map_err(|e| CoreError::CarError {
            operation: "finishing group CAR write".to_string(),
            cause: e,
        })?;

        Ok(manifest)
    }

    /// Export a constellation with all its agents and groups
    pub async fn export_constellation_to_car(
        &self,
        constellation_id: ConstellationId,
        mut output: impl AsyncWrite + Unpin + Send,
        options: ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();
        // Load the constellation with all its data (direct agents + groups with their agents)
        let (constellation, _all_groups, all_agents) =
            self.load_constellation_complete(&constellation_id).await?;

        // We'll use the constellation as the root of our CAR file
        let mut agent_export_cids = Vec::new();
        let mut all_blocks = Vec::new();
        let mut total_stats = ExportStats {
            memory_count: 0,
            message_count: 0,
            chunk_count: 0,
            total_blocks: 0,
            uncompressed_size: 0,
            compressed_size: None,
        };

        // Export all agents (from direct membership + groups)
        for agent in &all_agents {
            let (agent_export, agent_blocks, stats) =
                self.export_agent_to_blocks(agent, &options).await?;

            // Serialize the agent export and get its CID
            let agent_export_data =
                encode_dag_cbor(&agent_export).map_err(|e| CoreError::DagCborEncodingError {
                    data_type: "AgentExport".to_string(),
                    cause: e,
                })?;
            let agent_export_cid = Self::create_cid(&agent_export_data)?;

            agent_export_cids.push((agent.id.clone(), agent_export_cid));
            all_blocks.push((agent_export_cid, agent_export_data));
            all_blocks.extend(agent_blocks);

            // Accumulate stats
            total_stats.memory_count += stats.memory_count;
            total_stats.message_count += stats.message_count;
            total_stats.chunk_count += stats.chunk_count;
            total_stats.total_blocks += stats.total_blocks;
            total_stats.uncompressed_size += stats.uncompressed_size;
        }

        // Export all groups in the constellation
        let mut group_exports = Vec::new();
        for group_id in &constellation.groups {
            let group = self.load_group_with_members(group_id).await?;

            let group_export = self.export_group(&group, &agent_export_cids).await?;

            // Serialize group export
            let group_data =
                encode_dag_cbor(&group_export).map_err(|e| CoreError::DagCborEncodingError {
                    data_type: "GroupExport".to_string(),
                    cause: e,
                })?;
            if group_data.len() > MAX_BLOCK_BYTES {
                return Err(CoreError::CarError {
                    operation: "encoding GroupExport".to_string(),
                    cause: iroh_car::Error::Parsing("group export block too large".to_string()),
                });
            }
            let group_cid = Self::create_cid(&group_data)?;

            all_blocks.push((group_cid, group_data));
            total_stats.total_blocks += 1;

            group_exports.push(group_export);
        }

        // Create constellation export (slim: do not embed full agents inline)
        let mut constellation_slim = constellation.clone();
        // Preserve the membership data before clearing
        let agent_memberships: Vec<(AgentId, crate::groups::ConstellationMembership)> =
            constellation
                .agents
                .iter()
                .map(|(agent, membership)| (agent.id.clone(), membership.clone()))
                .collect();
        constellation_slim.agents.clear();
        let constellation_export = ConstellationExport {
            constellation: constellation_slim,
            groups: group_exports,
            agent_export_cids,
            agent_memberships,
        };

        // Serialize constellation export
        let constellation_data = encode_dag_cbor(&constellation_export).map_err(|e| {
            CoreError::DagCborEncodingError {
                data_type: "ConstellationExport".to_string(),
                cause: e,
            }
        })?;
        if constellation_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding ConstellationExport".to_string(),
                cause: iroh_car::Error::Parsing("constellation export block too large".to_string()),
            });
        }
        let constellation_cid = Self::create_cid(&constellation_data)?;

        total_stats.total_blocks += 1; // For the constellation export itself

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Constellation,
            stats: total_stats,
            data_cid: constellation_cid,
        };

        // Serialize manifest
        let manifest_data =
            encode_dag_cbor(&manifest).map_err(|e| CoreError::DagCborEncodingError {
                data_type: "ExportManifest".to_string(),
                cause: e,
            })?;
        if manifest_data.len() > MAX_BLOCK_BYTES {
            return Err(CoreError::CarError {
                operation: "encoding ExportManifest".to_string(),
                cause: iroh_car::Error::Parsing("manifest block too large".to_string()),
            });
        }
        let manifest_cid = Self::create_cid(&manifest_data)?;

        // Create CAR file with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut car_writer = CarWriter::new(header, &mut output);

        // Write manifest first
        car_writer
            .write(manifest_cid, &manifest_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing manifest to CAR".to_string(),
                cause: e,
            })?;

        // Write constellation block
        car_writer
            .write(constellation_cid, &constellation_data)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing constellation to CAR".to_string(),
                cause: e,
            })?;

        // Write all collected blocks
        for (cid, data) in all_blocks {
            car_writer
                .write(cid, &data)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "writing block to CAR".to_string(),
                    cause: e,
                })?;
        }

        car_writer.finish().await.map_err(|e| CoreError::CarError {
            operation: "finishing constellation CAR write".to_string(),
            cause: e,
        })?;

        Ok(manifest)
    }

    /// Export a group with references to its member agents
    async fn export_group(
        &self,
        group: &AgentGroup,
        agent_cids: &[(AgentId, Cid)],
    ) -> Result<GroupExport> {
        // Map member agent IDs to their export CIDs and preserve membership data
        let mut member_agent_cids: Vec<(AgentId, Cid)> = Vec::new();
        let mut member_memberships: Vec<(AgentId, GroupMembership)> = Vec::new();

        for (agent, membership) in &group.members {
            // Find the CID for this agent
            if let Some((_, cid)) = agent_cids.iter().find(|(id, _)| id == &agent.id) {
                member_agent_cids.push((agent.id.clone(), *cid));
                member_memberships.push((agent.id.clone(), membership.clone()));
            }
        }

        // Create a slim copy of the group without embedding full members inline
        let mut group_slim = group.clone();
        group_slim.members.clear();

        Ok(GroupExport {
            group: group_slim,
            member_agent_cids,
            member_memberships,
        })
    }

    /// Load constellation with all members and relations
    /// Load constellation with complete data: direct agents + all groups with their agents
    async fn load_constellation_complete(
        &self,
        constellation_id: &ConstellationId,
    ) -> Result<(Constellation, Vec<AgentGroup>, Vec<AgentRecord>)> {
        use crate::db::ops::get_entity;

        // First get the basic constellation
        let mut constellation = get_entity::<Constellation, _>(&self.db, constellation_id)
            .await?
            .ok_or_else(|| CoreError::AgentGroupError {
                group_name: "export".to_string(),
                operation: "load_constellation".to_string(),
                cause: format!("Constellation '{}' not found", constellation_id),
            })?;

        let mut all_agents = Vec::new();
        let mut all_groups = Vec::new();

        // Load direct constellation agents AND their membership data
        let memberships_query = r#"
            SELECT * FROM constellation_agents
            WHERE in = $constellation_id
            ORDER BY joined_at ASC
        "#;

        let mut result = self
            .db
            .query(memberships_query)
            .bind((
                "constellation_id",
                surrealdb::RecordId::from(constellation_id),
            ))
            .await
            .map_err(|e| CoreError::DatabaseQueryFailed {
                query: memberships_query.to_string(),
                table: "constellation_agents".to_string(),
                cause: e.into(),
            })?;

        let membership_db_models: Vec<crate::groups::ConstellationMembershipDbModel> =
            result.take(0).map_err(|e| CoreError::DatabaseQueryFailed {
                query: memberships_query.to_string(),
                table: "constellation_agents".to_string(),
                cause: e.into(),
            })?;

        // Convert membership models and load agents
        let mut constellation_agents = Vec::new();
        for membership_model in membership_db_models {
            let membership =
                crate::groups::ConstellationMembership::from_db_model(membership_model)?;
            // Load the agent (out_id is the AgentId in constellation membership)
            if let Some(agent) = crate::db::ops::get_entity::<crate::agent_entity::AgentRecord, _>(
                &self.db,
                &membership.out_id,
            )
            .await?
            {
                constellation_agents.push((agent, membership));
            }
        }

        constellation.agents = constellation_agents.clone();

        // Extract just the agents for further processing
        let mut direct_agents: Vec<AgentRecord> = constellation_agents
            .into_iter()
            .map(|(agent, _)| agent)
            .collect();

        // Load memories and messages for direct agents too
        for agent in &mut direct_agents {
            let (messages_result, memories_result) = tokio::join!(
                agent.load_message_history(&self.db, false),
                crate::db::ops::get_agent_memories(&self.db, &agent.id)
            );

            // Handle results
            if let Ok(messages) = messages_result {
                agent.messages = messages;
            }

            if let Ok(memory_tuples) = memories_result {
                agent.memories = memory_tuples
                    .into_iter()
                    .map(|(memory_block, access_level)| {
                        use crate::id::RelationId;
                        let relation = crate::agent_entity::AgentMemoryRelation {
                            id: RelationId::nil(),
                            in_id: agent.id.clone(),
                            out_id: memory_block.id.clone(),
                            access_level,
                            created_at: chrono::Utc::now(),
                        };
                        (memory_block, relation)
                    })
                    .collect();
            }
        }

        all_agents.extend(direct_agents);

        // Load all groups and their agents
        for group_id in &constellation.groups {
            // Load the group with all its agent members using ops function (load_with_relations doesn't work properly)
            if let Some(group) =
                crate::db::ops::get_entity::<crate::groups::AgentGroup, _>(&self.db, group_id)
                    .await?
            {
                // Manually load group members like get_group_by_name does
                let mut group = group;
                let query = r#"
                    SELECT * FROM group_members
                    WHERE out = $group_id
                    ORDER BY joined_at ASC
                "#;
                let mut result = self
                    .db
                    .query(query)
                    .bind(("group_id", surrealdb::RecordId::from(group_id)))
                    .await
                    .map_err(|e| CoreError::DatabaseQueryFailed {
                        query: query.to_string(),
                        table: "group_members".to_string(),
                        cause: e.into(),
                    })?;

                let membership_db_models: Vec<crate::groups::GroupMembershipDbModel> =
                    result.take(0).map_err(|e| CoreError::DatabaseQueryFailed {
                        query: query.to_string(),
                        table: "group_members".to_string(),
                        cause: e.into(),
                    })?;

                // Convert membership models and load agents
                let mut members = Vec::new();
                for membership_model in membership_db_models {
                    let membership =
                        crate::groups::GroupMembership::from_db_model(membership_model)?;
                    // Load the agent (in_id is the AgentId in group membership)
                    if let Some(agent) = crate::db::ops::get_entity::<
                        crate::agent_entity::AgentRecord,
                        _,
                    >(&self.db, &membership.in_id)
                    .await?
                    {
                        members.push((agent, membership));
                    }
                }
                group.members = members;
                // Add all agents from this group
                for (agent, _membership) in &group.members {
                    // Load full agent with memories and messages manually (like CLI does)
                    if let Some(mut full_agent) =
                        AgentRecord::load_with_relations(&self.db, &agent.id).await?
                    {
                        // Load message history and memory blocks like the CLI
                        let (messages_result, memories_result) = tokio::join!(
                            full_agent.load_message_history(&self.db, false),
                            crate::db::ops::get_agent_memories(&self.db, &full_agent.id)
                        );

                        // Handle results
                        if let Ok(messages) = messages_result {
                            full_agent.messages = messages;
                        }

                        if let Ok(memory_tuples) = memories_result {
                            full_agent.memories = memory_tuples
                                .into_iter()
                                .map(|(memory_block, access_level)| {
                                    use crate::id::RelationId;
                                    let relation = crate::agent_entity::AgentMemoryRelation {
                                        id: RelationId::nil(),
                                        in_id: full_agent.id.clone(),
                                        out_id: memory_block.id.clone(),
                                        access_level,
                                        created_at: chrono::Utc::now(),
                                    };
                                    (memory_block, relation)
                                })
                                .collect();
                        }

                        all_agents.push(full_agent);
                    }
                }
                all_groups.push(group);
            }
        }

        // Deduplicate agents (in case same agent is in multiple groups)
        all_agents.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        all_agents.dedup_by(|a, b| a.id == b.id);

        Ok((constellation, all_groups, all_agents))
    }

    /// Load group with all members
    async fn load_group_with_members(&self, group_id: &GroupId) -> Result<AgentGroup> {
        use crate::db::ops::get_entity;

        // Get the base group
        let mut group = get_entity::<AgentGroup, _>(&self.db, group_id)
            .await?
            .ok_or_else(|| CoreError::AgentGroupError {
                group_name: "export".to_string(),
                operation: "load_group".to_string(),
                cause: format!("Group '{}' not found", group_id),
            })?;

        // Load members via group_members edge
        let query = r#"
            SELECT * FROM group_members
            WHERE out = $group_id
            ORDER BY joined_at ASC
        "#;

        let mut result = self
            .db
            .query(query)
            .bind(("group_id", surrealdb::RecordId::from(group_id)))
            .await
            .map_err(|e| CoreError::DatabaseQueryFailed {
                query: query.to_string(),
                table: "group_members".to_string(),
                cause: e.into(),
            })?;

        let membership_db_models: Vec<<GroupMembership as DbEntity>::DbModel> =
            result.take(0).map_err(|e| CoreError::DatabaseQueryFailed {
                query: query.to_string(),
                table: "group_members".to_string(),
                cause: e.into(),
            })?;

        let memberships: Vec<GroupMembership> = membership_db_models
            .into_iter()
            .map(|db_model| {
                GroupMembership::from_db_model(db_model)
                    .map_err(|e| CoreError::from(crate::db::DatabaseError::from(e)))
            })
            .collect::<Result<Vec<_>>>()?;

        // Load the agents for each membership
        let mut members = Vec::new();
        for membership in memberships {
            if let Some(mut agent) = AgentRecord::load_with_relations(&self.db, &membership.in_id)
                .await
                .map_err(|e| CoreError::from(e))?
            {
                // Load message history and memory blocks (like CLI does)
                let (messages_result, memories_result) = tokio::join!(
                    agent.load_message_history(&self.db, true),
                    crate::db::ops::get_agent_memories(&self.db, &agent.id)
                );

                // Handle results
                if let Ok(messages) = messages_result {
                    tracing::info!(
                        "Loaded {} messages for agent {}",
                        messages.len(),
                        agent.name
                    );
                    agent.messages = messages;
                }

                if let Ok(memory_tuples) = memories_result {
                    tracing::info!(
                        "Loaded {} memory blocks for agent {}",
                        memory_tuples.len(),
                        agent.name
                    );
                    agent.memories = memory_tuples
                        .into_iter()
                        .map(|(memory_block, access_level)| {
                            use crate::id::RelationId;
                            let relation = crate::agent_entity::AgentMemoryRelation {
                                id: RelationId::nil(),
                                in_id: agent.id.clone(),
                                out_id: memory_block.id.clone(),
                                access_level,
                                created_at: chrono::Utc::now(),
                            };
                            (memory_block, relation)
                        })
                        .collect();
                }

                members.push((agent, membership));
            }
        }

        group.members = members;
        Ok(group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::client;
    use crate::memory::{MemoryBlock, MemoryPermission, MemoryType};

    async fn make_agent_with_data(msg_count: usize, mem_count: usize) -> AgentRecord {
        use crate::id::RelationId;
        use crate::message::{AgentMessageRelation, Message, MessageRelationType};
        use chrono::Utc;

        let mut agent = AgentRecord {
            name: "ExportTest".to_string(),
            owner_id: crate::UserId::generate(),
            ..Default::default()
        };

        // Messages
        let mut msgs = Vec::with_capacity(msg_count);
        for i in 0..msg_count {
            tokio::time::sleep(std::time::Duration::from_micros(500)).await;
            let m = Message::user(format!("m{}", i));
            // Relation mirrors message ids/positions for ordering
            let rel = AgentMessageRelation {
                id: RelationId::nil(),
                in_id: agent.id.clone(),
                out_id: m.id.clone(),
                message_type: MessageRelationType::Active,
                position: m.position.clone(),
                added_at: Utc::now(),
                batch: m.batch.clone(),
                sequence_num: m.sequence_num,
                batch_type: m.batch_type,
            };
            msgs.push((m, rel));
        }
        agent.messages = msgs;

        // Memories
        let mut mems = Vec::with_capacity(mem_count);
        for i in 0..mem_count {
            let mb = MemoryBlock {
                owner_id: agent.owner_id.clone(),
                label: compact_str::format_compact!("mem{}", i),
                value: format!("value-{}", i),
                memory_type: MemoryType::Working,
                permission: MemoryPermission::Append,
                ..Default::default()
            };
            let rel = crate::agent_entity::AgentMemoryRelation {
                id: RelationId::nil(),
                in_id: agent.id.clone(),
                out_id: mb.id.clone(),
                access_level: MemoryPermission::Append,
                created_at: Utc::now(),
            };
            mems.push((mb, rel));
        }
        agent.memories = mems;
        agent
    }

    #[tokio::test]
    async fn export_chunk_linkage_and_size() {
        let db = client::create_test_db().await.unwrap();
        let exporter = AgentExporter::new(db);
        // Ensure we exceed both default chunk counts
        let agent = make_agent_with_data(2500, 250).await;

        let (export, blocks, stats) = exporter
            .export_agent_to_blocks(&agent, &ExportOptions::default())
            .await
            .unwrap();

        // Collect blocks for lookup
        let map: std::collections::HashMap<_, _> = blocks.iter().cloned().collect();

        // Verify no block exceeds limit
        for (_cid, data) in blocks.iter() {
            assert!(data.len() <= MAX_BLOCK_BYTES);
        }

        // Verify agent metadata block exists and decodes
        let meta_bytes = map.get(&export.agent_cid).expect("agent meta present");
        let meta: AgentRecordExport = serde_ipld_dagcbor::from_slice(meta_bytes).unwrap();
        assert_eq!(meta.id, agent.id);
        assert_eq!(meta.message_chunks.len(), export.message_chunk_cids.len());
        assert_eq!(meta.memory_chunks.len(), export.memory_chunk_cids.len());

        // Verify message next_chunk wiring
        for (i, cid) in export.message_chunk_cids.iter().enumerate() {
            let data = map.get(cid).expect("msg chunk present");
            let chunk: MessageChunk = serde_ipld_dagcbor::from_slice(data).unwrap();
            if i + 1 < export.message_chunk_cids.len() {
                assert_eq!(chunk.next_chunk, Some(export.message_chunk_cids[i + 1]));
            } else {
                assert!(chunk.next_chunk.is_none());
            }
        }

        // Verify memory next_chunk wiring
        for (i, cid) in export.memory_chunk_cids.iter().enumerate() {
            let data = map.get(cid).expect("mem chunk present");
            let chunk: MemoryChunk = serde_ipld_dagcbor::from_slice(data).unwrap();
            if i + 1 < export.memory_chunk_cids.len() {
                assert_eq!(chunk.next_chunk, Some(export.memory_chunk_cids[i + 1]));
            } else {
                assert!(chunk.next_chunk.is_none());
            }
        }

        // Sanity on stats
        assert!(stats.message_count as usize >= 2500);
        assert!(stats.memory_count as usize >= 250);
        assert!(stats.chunk_count >= 3);
    }
}
