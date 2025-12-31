//! Agent exporter for CAR archives.
//!
//! Exports agents with their memory blocks, messages, archival entries,
//! and archive summaries to CAR format for backup and portability.

use chrono::{DateTime, Utc};
use cid::Cid;
use iroh_car::{CarHeader, CarWriter};
use sqlx::SqlitePool;
use tokio::io::AsyncWrite;

use pattern_db::queries;

use std::collections::{HashMap, HashSet};

use super::{
    EXPORT_VERSION, MAX_BLOCK_BYTES, TARGET_CHUNK_BYTES,
    car::{chunk_bytes, encode_block, estimate_size},
    types::{
        AgentExport, AgentRecord, ArchivalEntryExport, ArchiveSummaryExport, ConstellationExport,
        ExportManifest, ExportOptions, ExportStats, ExportTarget, ExportType, GroupConfigExport,
        GroupExport, GroupExportThin, GroupMemberExport, GroupRecord, MemoryBlockExport,
        MessageChunk, MessageExport, SharedBlockAttachmentExport, SnapshotChunk,
    },
};
use crate::error::{CoreError, Result};

/// Collects (CID, data) pairs during export for later CAR writing.
#[derive(Debug, Default)]
pub struct BlockCollector {
    /// Collected blocks as (CID, encoded data) pairs.
    pub blocks: Vec<(Cid, Vec<u8>)>,
}

impl BlockCollector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a block to the collection.
    pub fn push(&mut self, cid: Cid, data: Vec<u8>) {
        self.blocks.push((cid, data));
    }

    /// Number of blocks collected.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the collector is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Total bytes of all collected blocks.
    pub fn total_bytes(&self) -> u64 {
        self.blocks.iter().map(|(_, data)| data.len() as u64).sum()
    }

    /// Consume and return all blocks.
    pub fn into_blocks(self) -> Vec<(Cid, Vec<u8>)> {
        self.blocks
    }
}

/// Agent exporter - exports agents to CAR archives.
pub struct Exporter {
    pool: SqlitePool,
}

impl Exporter {
    /// Create a new exporter with the given database pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Export an agent to a CAR file.
    ///
    /// Loads the agent, memory blocks, messages, archival entries, and archive
    /// summaries, then writes them to the output as a CAR archive.
    pub async fn export_agent<W: AsyncWrite + Unpin + Send>(
        &self,
        agent_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();

        // Load agent
        let agent = queries::get_agent(&self.pool, agent_id)
            .await?
            .ok_or_else(|| CoreError::AgentNotFound {
                identifier: agent_id.to_string(),
            })?;

        // Export agent data to blocks
        let (agent_export, blocks, stats) = self.export_agent_data(&agent, options).await?;

        // Write CAR file
        let manifest = self
            .write_car(
                output,
                &agent_export,
                blocks,
                stats,
                start_time,
                ExportType::Agent,
            )
            .await?;

        Ok(manifest)
    }

    /// Export a group to a CAR file.
    ///
    /// Exports the group configuration and optionally all member agent data.
    /// Use `ExportTarget::Group { thin: true }` to export only the configuration
    /// without agent data.
    pub async fn export_group<W: AsyncWrite + Unpin + Send>(
        &self,
        group_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();

        // Load group
        let group = queries::get_group(&self.pool, group_id)
            .await?
            .ok_or_else(|| CoreError::GroupNotFound {
                identifier: group_id.to_string(),
            })?;

        // Load members
        let members = queries::get_group_members(&self.pool, group_id).await?;

        // Check if thin export
        let is_thin = matches!(&options.target, ExportTarget::Group { thin: true, .. });

        if is_thin {
            // Thin export: just group config and member IDs
            let config_export = GroupConfigExport {
                group: GroupRecord::from(&group),
                member_agent_ids: members.iter().map(|m| m.agent_id.clone()).collect(),
            };

            let collector = BlockCollector::new();
            let mut stats = ExportStats::default();
            stats.group_count = 1;
            stats.agent_count = members.len() as u64;

            // Write CAR file with config export
            let manifest = self
                .write_car_generic(
                    output,
                    &config_export,
                    "GroupConfigExport",
                    collector,
                    stats,
                    start_time,
                    ExportType::Group,
                )
                .await?;

            Ok(manifest)
        } else {
            // Full export: include all agent data
            let mut collector = BlockCollector::new();
            let mut stats = ExportStats::default();
            stats.group_count = 1;

            let mut agent_exports = Vec::with_capacity(members.len());

            for member in &members {
                let agent = queries::get_agent(&self.pool, &member.agent_id)
                    .await?
                    .ok_or_else(|| CoreError::AgentNotFound {
                        identifier: member.agent_id.clone(),
                    })?;

                let (agent_export, agent_blocks, agent_stats) =
                    self.export_agent_data(&agent, options).await?;

                // Merge stats
                stats.agent_count += agent_stats.agent_count;
                stats.message_count += agent_stats.message_count;
                stats.memory_block_count += agent_stats.memory_block_count;
                stats.archival_entry_count += agent_stats.archival_entry_count;
                stats.archive_summary_count += agent_stats.archive_summary_count;
                stats.chunk_count += agent_stats.chunk_count;

                // Add agent blocks to collector
                for (cid, data) in agent_blocks.into_blocks() {
                    collector.push(cid, data);
                }

                agent_exports.push(agent_export);
            }

            // Export shared memory blocks for the group
            let member_agent_ids: Vec<String> =
                members.iter().map(|m| m.agent_id.clone()).collect();
            let (shared_memory_cids, shared_attachment_exports) = self
                .export_shared_memory_for_group(
                    group_id,
                    &member_agent_ids,
                    &mut collector,
                    &mut stats,
                )
                .await?;

            // Create group export with inline agents
            let group_export = GroupExport {
                group: GroupRecord::from(&group),
                members: members.iter().map(GroupMemberExport::from).collect(),
                agent_exports,
                shared_memory_cids,
                shared_attachment_exports,
            };

            stats.total_blocks = collector.len() as u64;
            stats.total_bytes = collector.total_bytes();

            // Write CAR file
            let manifest = self
                .write_car_generic(
                    output,
                    &group_export,
                    "GroupExport",
                    collector,
                    stats,
                    start_time,
                    ExportType::Group,
                )
                .await?;

            Ok(manifest)
        }
    }

    /// Export a full constellation to a CAR file.
    ///
    /// Exports all agents and groups for the given owner, with agent deduplication.
    /// Agents that belong to multiple groups are only exported once.
    pub async fn export_constellation<W: AsyncWrite + Unpin + Send>(
        &self,
        owner_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();

        // Load all agents and groups
        let agents = queries::list_agents(&self.pool).await?;
        let groups = queries::list_groups(&self.pool).await?;

        let mut collector = BlockCollector::new();
        let mut stats = ExportStats::default();

        // Export each agent and collect CIDs
        let mut agent_cid_map: HashMap<String, Cid> = HashMap::new();

        for agent in &agents {
            let (agent_export, agent_blocks, agent_stats) =
                self.export_agent_data(agent, options).await?;

            // Merge stats
            stats.agent_count += agent_stats.agent_count;
            stats.message_count += agent_stats.message_count;
            stats.memory_block_count += agent_stats.memory_block_count;
            stats.archival_entry_count += agent_stats.archival_entry_count;
            stats.archive_summary_count += agent_stats.archive_summary_count;
            stats.chunk_count += agent_stats.chunk_count;

            // Add agent blocks to collector
            for (cid, data) in agent_blocks.into_blocks() {
                collector.push(cid, data);
            }

            // Encode agent export and store CID
            let (agent_cid, agent_data) = encode_block(&agent_export, "AgentExport")?;
            collector.push(agent_cid, agent_data);
            agent_cid_map.insert(agent.id.clone(), agent_cid);
        }

        // Track which agents are in groups
        let mut agents_in_groups: HashSet<String> = HashSet::new();

        // Create thin group exports
        let mut group_exports: Vec<GroupExportThin> = Vec::with_capacity(groups.len());

        for group in &groups {
            let members = queries::get_group_members(&self.pool, &group.id).await?;

            // Collect agent CIDs for this group
            let agent_cids: Vec<Cid> = members
                .iter()
                .filter_map(|m| agent_cid_map.get(&m.agent_id).copied())
                .collect();

            // Track agents in groups
            for member in &members {
                agents_in_groups.insert(member.agent_id.clone());
            }

            // Export shared memory for this group
            let member_agent_ids: Vec<String> =
                members.iter().map(|m| m.agent_id.clone()).collect();
            let (shared_memory_cids, shared_attachment_exports) = self
                .export_shared_memory_for_group(
                    &group.id,
                    &member_agent_ids,
                    &mut collector,
                    &mut stats,
                )
                .await?;

            let group_export = GroupExportThin {
                group: GroupRecord::from(group),
                members: members.iter().map(GroupMemberExport::from).collect(),
                agent_cids,
                shared_memory_cids,
                shared_attachment_exports,
            };

            group_exports.push(group_export);
            stats.group_count += 1;
        }

        // Find standalone agents (not in any group)
        let standalone_agent_cids: Vec<Cid> = agent_cid_map
            .iter()
            .filter(|(agent_id, _)| !agents_in_groups.contains(*agent_id))
            .map(|(_, cid)| *cid)
            .collect();

        // Export all memory blocks (for blocks not already exported with agents)
        // and collect all shared attachments
        let all_blocks = queries::list_all_blocks(&self.pool).await?;
        let all_attachments = queries::list_all_shared_block_attachments(&self.pool).await?;

        // Track which blocks we've already exported via agents
        let mut exported_block_ids: HashSet<String> = HashSet::new();
        for agent in &agents {
            let agent_blocks = queries::list_blocks(&self.pool, &agent.id).await?;
            for block in agent_blocks {
                exported_block_ids.insert(block.id);
            }
        }

        // Export any blocks not already included (e.g., orphaned or system blocks)
        let mut all_memory_block_cids: Vec<Cid> = Vec::new();
        for block in &all_blocks {
            if !exported_block_ids.contains(&block.id) {
                let cid = self
                    .export_memory_block_by_ref(block, &mut collector)
                    .await?;
                all_memory_block_cids.push(cid);
                stats.memory_block_count += 1;
            }
        }

        // Convert attachments to export format
        let shared_attachments: Vec<SharedBlockAttachmentExport> = all_attachments
            .iter()
            .map(SharedBlockAttachmentExport::from)
            .collect();

        // Create constellation export
        let constellation_export = ConstellationExport {
            version: EXPORT_VERSION,
            owner_id: owner_id.to_string(),
            exported_at: start_time,
            agent_exports: agent_cid_map,
            group_exports,
            standalone_agent_cids,
            all_memory_block_cids,
            shared_attachments,
        };

        stats.total_blocks = collector.len() as u64;
        stats.total_bytes = collector.total_bytes();

        // Write CAR file
        let manifest = self
            .write_car_generic(
                output,
                &constellation_export,
                "ConstellationExport",
                collector,
                stats,
                start_time,
                ExportType::Constellation,
            )
            .await?;

        Ok(manifest)
    }

    /// Export agent data to blocks without writing a CAR file.
    ///
    /// Returns the AgentExport, collected blocks, and export statistics.
    pub async fn export_agent_data(
        &self,
        agent: &pattern_db::models::Agent,
        options: &ExportOptions,
    ) -> Result<(AgentExport, BlockCollector, ExportStats)> {
        let mut collector = BlockCollector::new();
        let mut stats = ExportStats::default();

        // Export memory blocks
        let memory_block_cids = self
            .export_memory_blocks(&agent.id, &mut collector, &mut stats)
            .await?;

        // Export messages if requested
        let message_chunk_cids = if options.include_messages {
            self.export_messages(&agent.id, options, &mut collector, &mut stats)
                .await?
        } else {
            Vec::new()
        };

        // Export archival entries if requested
        let archival_entry_cids = if options.include_archival {
            self.export_archival_entries(&agent.id, &mut collector, &mut stats)
                .await?
        } else {
            Vec::new()
        };

        // Export archive summaries
        let archive_summary_cids = self
            .export_archive_summaries(&agent.id, &mut collector, &mut stats)
            .await?;

        // Create agent export
        let agent_export = AgentExport {
            agent: AgentRecord::from(agent),
            message_chunk_cids,
            memory_block_cids,
            archival_entry_cids,
            archive_summary_cids,
        };

        stats.agent_count = 1;
        stats.total_blocks = collector.len() as u64;
        stats.total_bytes = collector.total_bytes();

        Ok((agent_export, collector, stats))
    }

    /// Export memory blocks for an agent.
    ///
    /// Large Loro snapshots are chunked to fit within block size limits.
    /// Chunks are written in reverse order so each links forward via next_cid.
    async fn export_memory_blocks(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let blocks = queries::list_blocks(&self.pool, agent_id).await?;
        let mut export_cids = Vec::with_capacity(blocks.len());

        for block in blocks {
            stats.memory_block_count += 1;

            // Check if snapshot needs chunking
            let snapshot = &block.loro_snapshot;
            let snapshot_chunk_cids = if snapshot.len() > TARGET_CHUNK_BYTES {
                // Chunk the snapshot
                self.chunk_snapshot(snapshot, collector)?
            } else {
                // Inline - no chunking needed, store full snapshot in the export
                Vec::new()
            };

            // Create memory block export
            let export = MemoryBlockExport::from_memory_block(
                &block,
                snapshot_chunk_cids.clone(),
                snapshot.len() as u64,
            );

            // If no chunking was done, we need to encode the snapshot inline
            // The MemoryBlockExport doesn't include the snapshot directly,
            // so we need to handle this case specially
            let (cid, data) = if snapshot_chunk_cids.is_empty() && !snapshot.is_empty() {
                // For small snapshots, create a single chunk and reference it
                let chunk = SnapshotChunk {
                    index: 0,
                    data: snapshot.clone(),
                    next_cid: None,
                };
                let (chunk_cid, chunk_data) = encode_block(&chunk, "SnapshotChunk")?;
                collector.push(chunk_cid, chunk_data);

                // Update export with the chunk CID
                let export_with_chunks = MemoryBlockExport::from_memory_block(
                    &block,
                    vec![chunk_cid],
                    snapshot.len() as u64,
                );
                encode_block(&export_with_chunks, "MemoryBlockExport")?
            } else {
                encode_block(&export, "MemoryBlockExport")?
            };

            collector.push(cid, data);
            export_cids.push(cid);
        }

        Ok(export_cids)
    }

    /// Chunk a large Loro snapshot into blocks linked via next_cid.
    ///
    /// Chunks are written in reverse order so each chunk can reference the next.
    fn chunk_snapshot(&self, snapshot: &[u8], collector: &mut BlockCollector) -> Result<Vec<Cid>> {
        let raw_chunks = chunk_bytes(snapshot, TARGET_CHUNK_BYTES);
        if raw_chunks.is_empty() {
            return Ok(Vec::new());
        }

        // Process chunks in reverse to wire forward links
        let mut chunk_cids = vec![Cid::default(); raw_chunks.len()];
        let mut next_cid: Option<Cid> = None;

        for (idx, chunk_data) in raw_chunks.iter().enumerate().rev() {
            let chunk = SnapshotChunk {
                index: idx as u32,
                data: chunk_data.clone(),
                next_cid,
            };

            let (cid, encoded) = encode_block(&chunk, "SnapshotChunk")?;
            collector.push(cid, encoded);
            chunk_cids[idx] = cid;
            next_cid = Some(cid);
        }

        Ok(chunk_cids)
    }

    /// Export messages for an agent in size-based chunks.
    ///
    /// Messages are grouped into chunks based on size limits. Each chunk
    /// references the next via next_cid (not applicable in current design,
    /// but CIDs are returned in order).
    async fn export_messages(
        &self,
        agent_id: &str,
        options: &ExportOptions,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        // Load all messages (including archived) - use a very high limit
        let messages = queries::get_messages_with_archived(&self.pool, agent_id, i64::MAX).await?;

        if messages.is_empty() {
            return Ok(Vec::new());
        }

        // Build message chunks based on size limits
        let mut pending_chunks: Vec<Vec<MessageExport>> = Vec::new();
        let mut current_chunk: Vec<MessageExport> = Vec::new();
        let mut current_size: usize = 0;

        for msg in messages {
            let export = MessageExport::from(&msg);
            let msg_size = estimate_size(&export)?;

            // Check if adding this message would exceed limits
            let would_exceed_size = current_size + msg_size > options.max_chunk_bytes;
            let would_exceed_count = current_chunk.len() >= options.max_messages_per_chunk;

            if !current_chunk.is_empty() && (would_exceed_size || would_exceed_count) {
                // Finalize current chunk
                pending_chunks.push(std::mem::take(&mut current_chunk));
                current_size = 0;
            }

            // Verify single message fits
            if msg_size > MAX_BLOCK_BYTES {
                return Err(CoreError::ExportError {
                    operation: "encoding message".to_string(),
                    cause: format!(
                        "single message exceeds block limit ({} > {})",
                        msg_size, MAX_BLOCK_BYTES
                    ),
                });
            }

            current_chunk.push(export);
            current_size += msg_size;
            stats.message_count += 1;
        }

        // Don't forget the last chunk
        if !current_chunk.is_empty() {
            pending_chunks.push(current_chunk);
        }

        // Encode chunks
        let mut chunk_cids = Vec::with_capacity(pending_chunks.len());
        for (idx, messages) in pending_chunks.iter().enumerate() {
            let start_position = messages
                .first()
                .map(|m| m.position.clone())
                .unwrap_or_default();
            let end_position = messages
                .last()
                .map(|m| m.position.clone())
                .unwrap_or_default();

            let chunk = MessageChunk {
                chunk_index: idx as u32,
                start_position,
                end_position,
                messages: messages.clone(),
                message_count: messages.len() as u32,
            };

            let (cid, data) = encode_block(&chunk, "MessageChunk")?;
            collector.push(cid, data);
            chunk_cids.push(cid);
        }

        stats.chunk_count = chunk_cids.len() as u64;
        Ok(chunk_cids)
    }

    /// Export archival entries for an agent.
    async fn export_archival_entries(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        // Load all archival entries (use high limit and offset 0)
        let entries = queries::list_archival_entries(&self.pool, agent_id, i64::MAX, 0).await?;

        let mut cids = Vec::with_capacity(entries.len());
        for entry in entries {
            stats.archival_entry_count += 1;
            let export = ArchivalEntryExport::from(&entry);
            let (cid, data) = encode_block(&export, "ArchivalEntryExport")?;
            collector.push(cid, data);
            cids.push(cid);
        }

        Ok(cids)
    }

    /// Export archive summaries for an agent.
    async fn export_archive_summaries(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let summaries = queries::get_archive_summaries(&self.pool, agent_id).await?;

        let mut cids = Vec::with_capacity(summaries.len());
        for summary in summaries {
            stats.archive_summary_count += 1;
            let export = ArchiveSummaryExport::from(&summary);
            let (cid, data) = encode_block(&export, "ArchiveSummaryExport")?;
            collector.push(cid, data);
            cids.push(cid);
        }

        Ok(cids)
    }

    /// Export shared memory blocks for a group.
    ///
    /// Collects blocks shared with group members (not owned by them) and the
    /// corresponding attachment records.
    ///
    /// Returns (shared_block_cids, shared_attachment_exports).
    async fn export_shared_memory_for_group(
        &self,
        group_id: &str,
        member_agent_ids: &[String],
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<(Vec<Cid>, Vec<SharedBlockAttachmentExport>)> {
        // Collect all blocks shared with group members
        let mut shared_block_ids: HashSet<String> = HashSet::new();
        let mut attachment_exports: Vec<SharedBlockAttachmentExport> = Vec::new();

        for agent_id in member_agent_ids {
            // Get blocks shared WITH this agent (not owned by them)
            let attachments = queries::list_agent_shared_blocks(&self.pool, agent_id).await?;
            for attachment in attachments {
                shared_block_ids.insert(attachment.block_id.clone());
                attachment_exports.push(SharedBlockAttachmentExport::from(&attachment));
            }
        }

        // Also get blocks owned by the group itself
        let group_blocks = queries::list_blocks(&self.pool, group_id).await?;

        // Export the shared blocks (avoiding duplicates with agent-owned blocks)
        let mut shared_cids = Vec::new();
        for block_id in &shared_block_ids {
            if let Some(block) = queries::get_block(&self.pool, block_id).await? {
                // Check if this block is already exported as part of an agent's blocks
                // by checking if the owner is in our member list
                if !member_agent_ids.contains(&block.agent_id) {
                    // This block is from outside the group, export it
                    let snapshot = &block.loro_snapshot;
                    let snapshot_chunk_cids = if snapshot.len() > TARGET_CHUNK_BYTES {
                        self.chunk_snapshot(snapshot, collector)?
                    } else if !snapshot.is_empty() {
                        // Create a single chunk for small snapshots
                        let chunk = SnapshotChunk {
                            index: 0,
                            data: snapshot.clone(),
                            next_cid: None,
                        };
                        let (chunk_cid, chunk_data) = encode_block(&chunk, "SnapshotChunk")?;
                        collector.push(chunk_cid, chunk_data);
                        vec![chunk_cid]
                    } else {
                        Vec::new()
                    };

                    let export = MemoryBlockExport::from_memory_block(
                        &block,
                        snapshot_chunk_cids,
                        snapshot.len() as u64,
                    );
                    let (cid, data) = encode_block(&export, "MemoryBlockExport")?;
                    collector.push(cid, data);
                    shared_cids.push(cid);
                    stats.memory_block_count += 1;
                }
            }
        }

        // Export group-owned blocks
        for block in group_blocks {
            let snapshot = &block.loro_snapshot;
            let snapshot_chunk_cids = if snapshot.len() > TARGET_CHUNK_BYTES {
                self.chunk_snapshot(snapshot, collector)?
            } else if !snapshot.is_empty() {
                let chunk = SnapshotChunk {
                    index: 0,
                    data: snapshot.clone(),
                    next_cid: None,
                };
                let (chunk_cid, chunk_data) = encode_block(&chunk, "SnapshotChunk")?;
                collector.push(chunk_cid, chunk_data);
                vec![chunk_cid]
            } else {
                Vec::new()
            };

            let export = MemoryBlockExport::from_memory_block(
                &block,
                snapshot_chunk_cids,
                snapshot.len() as u64,
            );
            let (cid, data) = encode_block(&export, "MemoryBlockExport")?;
            collector.push(cid, data);
            shared_cids.push(cid);
            stats.memory_block_count += 1;
        }

        Ok((shared_cids, attachment_exports))
    }

    /// Export a single memory block by reference.
    ///
    /// Used for exporting blocks that aren't part of an agent's owned blocks.
    async fn export_memory_block_by_ref(
        &self,
        block: &pattern_db::models::MemoryBlock,
        collector: &mut BlockCollector,
    ) -> Result<Cid> {
        let snapshot = &block.loro_snapshot;
        let snapshot_chunk_cids = if snapshot.len() > TARGET_CHUNK_BYTES {
            self.chunk_snapshot(snapshot, collector)?
        } else if !snapshot.is_empty() {
            let chunk = SnapshotChunk {
                index: 0,
                data: snapshot.clone(),
                next_cid: None,
            };
            let (chunk_cid, chunk_data) = encode_block(&chunk, "SnapshotChunk")?;
            collector.push(chunk_cid, chunk_data);
            vec![chunk_cid]
        } else {
            Vec::new()
        };

        let export =
            MemoryBlockExport::from_memory_block(block, snapshot_chunk_cids, snapshot.len() as u64);
        let (cid, data) = encode_block(&export, "MemoryBlockExport")?;
        collector.push(cid, data);
        Ok(cid)
    }

    /// Write blocks to a CAR file.
    ///
    /// The manifest is written as the root block, followed by the export data
    /// and all collected blocks.
    async fn write_car<W: AsyncWrite + Unpin + Send>(
        &self,
        mut output: W,
        agent_export: &AgentExport,
        collector: BlockCollector,
        stats: ExportStats,
        exported_at: DateTime<Utc>,
        export_type: ExportType,
    ) -> Result<ExportManifest> {
        // Encode the agent export
        let (data_cid, data_bytes) = encode_block(agent_export, "AgentExport")?;

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at,
            export_type,
            stats,
            data_cid,
        };

        let (manifest_cid, manifest_bytes) = encode_block(&manifest, "ExportManifest")?;

        // Create CAR writer with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut writer = CarWriter::new(header, &mut output);

        // Write manifest first
        writer
            .write(manifest_cid, &manifest_bytes)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing manifest".to_string(),
                cause: e,
            })?;

        // Write agent export data
        writer
            .write(data_cid, &data_bytes)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing agent export".to_string(),
                cause: e,
            })?;

        // Write all collected blocks
        for (cid, data) in collector.into_blocks() {
            writer
                .write(cid, &data)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "writing block".to_string(),
                    cause: e,
                })?;
        }

        // Finish the CAR file
        writer.finish().await.map_err(|e| CoreError::CarError {
            operation: "finishing CAR".to_string(),
            cause: e,
        })?;

        Ok(manifest)
    }

    /// Write blocks to a CAR file with a generic data type.
    ///
    /// Like `write_car` but accepts any serializable type as the data payload.
    async fn write_car_generic<W: AsyncWrite + Unpin + Send, T: serde::Serialize>(
        &self,
        mut output: W,
        data: &T,
        type_name: &str,
        collector: BlockCollector,
        stats: ExportStats,
        exported_at: DateTime<Utc>,
        export_type: ExportType,
    ) -> Result<ExportManifest> {
        // Encode the data
        let (data_cid, data_bytes) = encode_block(data, type_name)?;

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at,
            export_type,
            stats,
            data_cid,
        };

        let (manifest_cid, manifest_bytes) = encode_block(&manifest, "ExportManifest")?;

        // Create CAR writer with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut writer = CarWriter::new(header, &mut output);

        // Write manifest first
        writer
            .write(manifest_cid, &manifest_bytes)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "writing manifest".to_string(),
                cause: e,
            })?;

        // Write data
        writer
            .write(data_cid, &data_bytes)
            .await
            .map_err(|e| CoreError::CarError {
                operation: format!("writing {}", type_name),
                cause: e,
            })?;

        // Write all collected blocks
        for (cid, data) in collector.into_blocks() {
            writer
                .write(cid, &data)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "writing block".to_string(),
                    cause: e,
                })?;
        }

        // Finish the CAR file
        writer.finish().await.map_err(|e| CoreError::CarError {
            operation: "finishing CAR".to_string(),
            cause: e,
        })?;

        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::super::car::create_cid;
    use super::*;
    use pattern_db::ConstellationDb;

    async fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_block_collector() {
        let mut collector = BlockCollector::new();
        assert!(collector.is_empty());
        assert_eq!(collector.len(), 0);
        assert_eq!(collector.total_bytes(), 0);

        // Add a block
        let data = vec![1, 2, 3, 4, 5];
        let cid = create_cid(&data);
        collector.push(cid, data.clone());

        assert!(!collector.is_empty());
        assert_eq!(collector.len(), 1);
        assert_eq!(collector.total_bytes(), 5);

        // Consume blocks
        let blocks = collector.into_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, cid);
        assert_eq!(blocks[0].1, data);
    }

    #[tokio::test]
    async fn test_exporter_new() {
        let db = setup_test_db().await;
        let _exporter = Exporter::new(db.pool().clone());
        // Basic construction test
    }

    #[tokio::test]
    async fn test_chunk_snapshot_small() {
        let db = setup_test_db().await;
        let exporter = Exporter::new(db.pool().clone());

        // Small snapshot that doesn't need chunking
        let snapshot = vec![1, 2, 3, 4, 5];
        let mut collector = BlockCollector::new();

        let cids = exporter.chunk_snapshot(&snapshot, &mut collector).unwrap();

        // Should produce one chunk
        assert_eq!(cids.len(), 1);
        assert_eq!(collector.len(), 1);
    }

    #[tokio::test]
    async fn test_export_nonexistent_agent() {
        let db = setup_test_db().await;
        let exporter = Exporter::new(db.pool().clone());

        let mut output = Vec::new();
        let options = ExportOptions::default();

        let result = exporter
            .export_agent("nonexistent-agent-id", &mut output, &options)
            .await;

        assert!(result.is_err());
        match result {
            Err(CoreError::AgentNotFound { identifier }) => {
                assert_eq!(identifier, "nonexistent-agent-id");
            }
            _ => panic!("Expected AgentNotFound error"),
        }
    }
}
