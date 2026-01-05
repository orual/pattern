//! CAR v1/v2 to v3 converter.
//!
//! This module provides functionality to convert legacy CAR exports (v1/v2)
//! to the current v3 format used by pattern_core's export system.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use cid::Cid;
use iroh_car::CarReader;
use serde_ipld_dagcbor::from_slice;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::BufReader;
use tracing::info;

// V3 types from pattern_core::export
use pattern_core::export::{
    AgentExport, AgentRecord, ArchivalEntryExport, ArchiveSummaryExport, EXPORT_VERSION,
    ExportManifest, ExportStats, ExportType, GroupExport, GroupMemberExport, GroupRecord,
    MemoryBlockExport, MessageChunk, MessageExport, SharedBlockAttachmentExport, SnapshotChunk,
    TARGET_CHUNK_BYTES, encode_block,
};

// pattern_db types for enum mappings
use pattern_db::models::{
    AgentStatus, BatchType, GroupMemberRole, MemoryBlockType, MemoryPermission, MessageRole,
    PatternType,
};

// Old types from this crate
use crate::export::{
    AgentExport as OldAgentExport, AgentRecordExport as OldAgentRecord,
    ConstellationExport as OldConstellationExport, ExportManifest as OldManifest,
    ExportType as OldExportType, GroupExport as OldGroupExport, MemoryChunk as OldMemoryChunk,
    MessageChunk as OldMessageChunk,
};
use crate::groups::{AgentGroup, CoordinationPattern};
use crate::memory::{MemoryBlock as OldMemoryBlock, MemoryPermission as OldPermission, MemoryType};
use crate::message::{
    AgentMessageRelation, BatchType as OldBatchType, ChatRole, Message as OldMessage,
    MessageRelationType,
};

/// Options for CAR conversion.
#[derive(Debug, Clone)]
pub struct ConversionOptions {
    /// Convert archival memory blocks to archival entries.
    /// When true, old archival blocks become ArchivalEntryExport (searchable text).
    /// When false, they remain as MemoryBlockExport (Loro CRDT).
    pub archival_blocks_to_entries: bool,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            archival_blocks_to_entries: true,
        }
    }
}

/// Errors that can occur during CAR conversion.
#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CAR read error: {0}")]
    CarRead(String),

    #[error("CBOR decode error: {0}")]
    CborDecode(String),

    #[error("CBOR encode error: {0}")]
    CborEncode(String),

    #[error("CID not found: {0}")]
    CidNotFound(String),

    #[error("Unsupported export version: {0}")]
    UnsupportedVersion(u32),

    #[error("Invalid export type for conversion")]
    InvalidExportType,

    #[error("Missing required data: {0}")]
    MissingData(String),

    #[error("Core error: {0}")]
    Core(String),
}

/// Statistics about a CAR conversion.
#[derive(Debug, Clone, Default)]
pub struct ConversionStats {
    /// Number of agents converted
    pub agents_converted: u64,
    /// Number of messages converted
    pub messages_converted: u64,
    /// Number of memory blocks converted
    pub memory_blocks_converted: u64,
    /// Number of archival entries converted
    pub archival_entries_converted: u64,
    /// Number of groups converted
    pub groups_converted: u64,
    /// Input format version
    pub input_version: u32,
}

/// Convert a v1/v2 CAR file to v3 format.
///
/// Reads the old CAR file, parses the manifest, converts all data
/// to v3 format, and writes a new CAR file.
pub async fn convert_car_v1v2_to_v3(
    input_path: &Path,
    output_path: &Path,
    options: &ConversionOptions,
) -> Result<ConversionStats, ConversionError> {
    info!(
        "Converting CAR file from {} to {}",
        input_path.display(),
        output_path.display()
    );

    // Read the old CAR file
    let file = File::open(input_path).await?;
    let reader = BufReader::new(file);

    let mut car_reader = CarReader::new(reader)
        .await
        .map_err(|e| ConversionError::CarRead(e.to_string()))?;

    // Collect all blocks into a map for random access
    let mut blocks: HashMap<Cid, Vec<u8>> = HashMap::new();
    let roots = car_reader.header().roots().to_vec();

    while let Some((cid, data)) = car_reader
        .next_block()
        .await
        .map_err(|e| ConversionError::CarRead(e.to_string()))?
    {
        blocks.insert(cid, data);
    }

    // Get the root block (manifest)
    let root_cid = roots
        .first()
        .ok_or_else(|| ConversionError::MissingData("No root CID in CAR file".to_string()))?;

    let manifest_data = blocks
        .get(root_cid)
        .ok_or_else(|| ConversionError::CidNotFound(root_cid.to_string()))?;

    let old_manifest: OldManifest =
        from_slice(manifest_data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

    // Verify version
    if old_manifest.version >= 3 {
        return Err(ConversionError::UnsupportedVersion(old_manifest.version));
    }

    info!(
        "Found v{} {:?} export from {}",
        old_manifest.version, old_manifest.export_type, old_manifest.exported_at
    );

    // Convert based on export type
    let mut stats = ConversionStats {
        input_version: old_manifest.version,
        ..Default::default()
    };

    // Storage for converted blocks
    let mut converted_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();

    let (data_cid, new_export_type) = match old_manifest.export_type {
        OldExportType::Agent => {
            let result = convert_agent_export(&blocks, &old_manifest.data_cid, options)?;
            converted_blocks.extend(result.blocks);
            stats.agents_converted = 1;
            stats.messages_converted = result.message_count;
            stats.memory_blocks_converted = result.memory_count;
            stats.archival_entries_converted = result.archival_count;
            (result.export_cid, ExportType::Agent)
        }
        OldExportType::Group => {
            let (cid, group_blocks, group_stats) =
                convert_group_export(&blocks, &old_manifest.data_cid, options)?;
            converted_blocks.extend(group_blocks);
            stats.groups_converted = 1;
            stats.agents_converted = group_stats.0;
            stats.messages_converted = group_stats.1;
            stats.memory_blocks_converted = group_stats.2;
            stats.archival_entries_converted = group_stats.3;
            (cid, ExportType::Group)
        }
        OldExportType::Constellation => {
            let (cid, const_blocks, const_stats) =
                convert_constellation_export(&blocks, &old_manifest.data_cid, options)?;
            converted_blocks.extend(const_blocks);
            stats.agents_converted = const_stats.0;
            stats.groups_converted = const_stats.1;
            stats.messages_converted = const_stats.2;
            stats.memory_blocks_converted = const_stats.3;
            stats.archival_entries_converted = const_stats.4;
            (cid, ExportType::Constellation)
        }
    };

    // Create new manifest
    let new_manifest = ExportManifest {
        version: EXPORT_VERSION,
        exported_at: Utc::now(),
        export_type: new_export_type,
        stats: ExportStats {
            agent_count: stats.agents_converted,
            group_count: stats.groups_converted,
            message_count: stats.messages_converted,
            memory_block_count: stats.memory_blocks_converted,
            archival_entry_count: 0, // Old format doesn't track separately
            archive_summary_count: 0,
            chunk_count: 0,                                  // Will be updated
            total_blocks: converted_blocks.len() as u64 + 1, // +1 for manifest
            total_bytes: 0,                                  // Will be calculated
        },
        data_cid,
    };

    let (manifest_cid, manifest_data) = encode_block(&new_manifest, "ExportManifest")
        .map_err(|e| ConversionError::Core(e.to_string()))?;

    // Write the new CAR file
    write_car_file(output_path, manifest_cid, manifest_data, converted_blocks).await?;

    info!(
        "Conversion complete: {} agents, {} messages, {} memory blocks",
        stats.agents_converted, stats.messages_converted, stats.memory_blocks_converted
    );

    Ok(stats)
}

/// Result of converting an agent export.
struct AgentConversionResult {
    /// CID of the converted AgentExport block
    export_cid: Cid,
    /// All encoded blocks for this agent
    blocks: Vec<(Cid, Vec<u8>)>,
    /// Message count
    message_count: u64,
    /// Memory block count (core/working only)
    memory_count: u64,
    /// Archival entry count (converted from archival blocks)
    archival_count: u64,
    /// All memory relations from this agent's export
    memory_relations: Vec<CollectedMemoryRelation>,
}

/// Convert an agent export from v1/v2 to v3.
fn convert_agent_export(
    blocks: &HashMap<Cid, Vec<u8>>,
    data_cid: &Cid,
    options: &ConversionOptions,
) -> Result<AgentConversionResult, ConversionError> {
    let data = blocks
        .get(data_cid)
        .ok_or_else(|| ConversionError::CidNotFound(data_cid.to_string()))?;

    let old_export: OldAgentExport =
        from_slice(data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

    let mut new_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut message_count = 0u64;
    let mut memory_count = 0u64;
    let mut all_relations: Vec<CollectedMemoryRelation> = Vec::new();

    // Load the agent record
    let agent_data = blocks
        .get(&old_export.agent_cid)
        .ok_or_else(|| ConversionError::CidNotFound(old_export.agent_cid.to_string()))?;

    let old_agent: OldAgentRecord =
        from_slice(agent_data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

    // Convert message chunks (may produce more chunks than input due to re-chunking)
    let mut message_chunk_cids: Vec<Cid> = Vec::new();
    let mut next_chunk_index: u32 = 0;
    for msg_chunk_cid in &old_export.message_chunk_cids {
        let chunk_data = blocks
            .get(msg_chunk_cid)
            .ok_or_else(|| ConversionError::CidNotFound(msg_chunk_cid.to_string()))?;

        let old_chunk: OldMessageChunk =
            from_slice(chunk_data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

        let (chunk_cids, chunk_blocks, count, chunks_created) =
            convert_message_chunk(&old_chunk, &old_agent.id.to_string(), next_chunk_index)?;
        message_count += count;
        new_blocks.extend(chunk_blocks);
        message_chunk_cids.extend(chunk_cids);
        next_chunk_index += chunks_created;
    }

    // Convert memory chunks
    let mut memory_block_cids: Vec<Cid> = Vec::new();
    let mut archival_entry_cids: Vec<Cid> = Vec::new();
    let mut archival_count = 0u64;
    for mem_chunk_cid in &old_export.memory_chunk_cids {
        let chunk_data = blocks
            .get(mem_chunk_cid)
            .ok_or_else(|| ConversionError::CidNotFound(mem_chunk_cid.to_string()))?;

        let old_chunk: OldMemoryChunk =
            from_slice(chunk_data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

        let result = convert_memory_chunk(&old_chunk, &old_agent.id.to_string(), options)?;
        memory_count += result.memory_count;
        archival_count += result.archival_count;
        new_blocks.extend(result.blocks);
        memory_block_cids.extend(result.memory_block_cids);
        archival_entry_cids.extend(result.archival_entry_cids);
        all_relations.extend(result.relations);
    }

    // Convert agent record
    let new_agent = convert_agent_record(&old_agent)?;

    // Convert message_summary to archive summaries
    let archive_summary_cids = if let Some(ref summary) = old_agent.message_summary {
        let (sum_cids, sum_blocks) =
            convert_message_summary(summary, &old_agent.id.to_string(), None, None)?;
        new_blocks.extend(sum_blocks);
        sum_cids
    } else {
        Vec::new()
    };

    // Create the v3 AgentExport
    let new_export = AgentExport {
        agent: new_agent,
        message_chunk_cids,
        memory_block_cids,
        archival_entry_cids,
        archive_summary_cids,
    };

    let (export_cid, export_data) = encode_block(&new_export, "AgentExport")
        .map_err(|e| ConversionError::Core(e.to_string()))?;
    new_blocks.push((export_cid, export_data));

    Ok(AgentConversionResult {
        export_cid,
        blocks: new_blocks,
        message_count,
        memory_count,
        archival_count,
        memory_relations: all_relations,
    })
}

/// Convert a group export from v1/v2 to v3.
fn convert_group_export(
    blocks: &HashMap<Cid, Vec<u8>>,
    data_cid: &Cid,
    options: &ConversionOptions,
) -> Result<(Cid, Vec<(Cid, Vec<u8>)>, (u64, u64, u64, u64)), ConversionError> {
    let data = blocks
        .get(data_cid)
        .ok_or_else(|| ConversionError::CidNotFound(data_cid.to_string()))?;

    let old_export: OldGroupExport =
        from_slice(data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

    let mut new_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut agent_count = 0u64;
    let mut message_count = 0u64;
    let mut memory_count = 0u64;
    let mut archival_count = 0u64;
    let mut all_relations: Vec<CollectedMemoryRelation> = Vec::new();

    // Convert member agents
    let mut agent_exports: Vec<AgentExport> = Vec::new();
    for (_agent_id, agent_cid) in &old_export.member_agent_cids {
        let result = convert_agent_export(blocks, agent_cid, options)?;
        new_blocks.extend(result.blocks);
        agent_count += 1;
        message_count += result.message_count;
        memory_count += result.memory_count;
        archival_count += result.archival_count;
        all_relations.extend(result.memory_relations);

        // Load the converted agent export
        let agent_data = new_blocks
            .iter()
            .find(|(cid, _)| cid == &result.export_cid)
            .map(|(_, data)| data.clone())
            .ok_or_else(|| ConversionError::CidNotFound(result.export_cid.to_string()))?;

        let agent_export: AgentExport =
            from_slice(&agent_data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;
        agent_exports.push(agent_export);
    }

    // Convert group record
    let new_group_record = convert_group_record(&old_export.group)?;

    // Convert member memberships
    let members: Vec<GroupMemberExport> = old_export
        .member_memberships
        .iter()
        .map(|(agent_id, membership)| GroupMemberExport {
            group_id: old_export.group.id.to_string(),
            agent_id: agent_id.to_string(),
            role: convert_group_member_role(&membership.role),
            capabilities: membership.capabilities.clone(),
            joined_at: membership.joined_at,
        })
        .collect();

    // Build shared block attachments from collected relations
    // A block is "shared" if it appears in multiple agents' relations
    let (shared_memory_id_cids, shared_attachment_exports) =
        build_shared_block_exports(&all_relations, &new_blocks)?;

    // Extract just the CIDs for the export (drop the block_id keys)
    let shared_memory_cids: Vec<Cid> = shared_memory_id_cids
        .into_iter()
        .map(|(_, cid)| cid)
        .collect();

    // Create the v3 GroupExport
    let new_export = GroupExport {
        group: new_group_record,
        members,
        agent_exports,
        shared_memory_cids,
        shared_attachment_exports,
    };

    let (export_cid, export_data) = encode_block(&new_export, "GroupExport")
        .map_err(|e| ConversionError::Core(e.to_string()))?;
    new_blocks.push((export_cid, export_data));

    Ok((
        export_cid,
        new_blocks,
        (agent_count, message_count, memory_count, archival_count),
    ))
}

/// Convert a constellation export from v1/v2 to v3.
fn convert_constellation_export(
    blocks: &HashMap<Cid, Vec<u8>>,
    data_cid: &Cid,
    options: &ConversionOptions,
) -> Result<(Cid, Vec<(Cid, Vec<u8>)>, (u64, u64, u64, u64, u64)), ConversionError> {
    let data = blocks
        .get(data_cid)
        .ok_or_else(|| ConversionError::CidNotFound(data_cid.to_string()))?;

    let old_export: OldConstellationExport =
        from_slice(data).map_err(|e| ConversionError::CborDecode(e.to_string()))?;

    let mut new_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut agent_count = 0u64;
    let mut group_count = 0u64;
    let mut message_count = 0u64;
    let mut memory_count = 0u64;
    let mut archival_count = 0u64;
    let mut all_relations: Vec<CollectedMemoryRelation> = Vec::new();

    // Convert all agents
    let mut agent_exports: HashMap<String, Cid> = HashMap::new();
    for (agent_id, agent_cid) in &old_export.agent_export_cids {
        let result = convert_agent_export(blocks, agent_cid, options)?;
        new_blocks.extend(result.blocks);
        agent_count += 1;
        message_count += result.message_count;
        memory_count += result.memory_count;
        archival_count += result.archival_count;
        all_relations.extend(result.memory_relations);
        agent_exports.insert(agent_id.to_string(), result.export_cid);
    }

    // Build shared block attachments from collected relations
    let (shared_memory_cids, shared_attachments) =
        build_shared_block_exports(&all_relations, &new_blocks)?;

    // Convert all groups
    let mut group_export_thins: Vec<pattern_core::export::GroupExportThin> = Vec::new();
    for old_group_export in &old_export.groups {
        let new_group_record = convert_group_record(&old_group_export.group)?;
        group_count += 1;

        // Get member CIDs from our converted agents
        let agent_cids: Vec<Cid> = old_group_export
            .member_agent_cids
            .iter()
            .filter_map(|(agent_id, _)| agent_exports.get(&agent_id.to_string()).copied())
            .collect();

        // Convert members
        let members: Vec<GroupMemberExport> = old_group_export
            .member_memberships
            .iter()
            .map(|(agent_id, membership)| GroupMemberExport {
                group_id: old_group_export.group.id.to_string(),
                agent_id: agent_id.to_string(),
                role: convert_group_member_role(&membership.role),
                capabilities: membership.capabilities.clone(),
                joined_at: membership.joined_at,
            })
            .collect();

        // Filter shared blocks/attachments for this group's members
        let group_member_ids: std::collections::HashSet<_> = old_group_export
            .member_agent_cids
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();

        let group_shared_attachments: Vec<SharedBlockAttachmentExport> = shared_attachments
            .iter()
            .filter(|a| group_member_ids.contains(&a.agent_id))
            .cloned()
            .collect();

        let group_shared_block_ids: std::collections::HashSet<_> = group_shared_attachments
            .iter()
            .map(|a| a.block_id.clone())
            .collect();

        let group_shared_cids: Vec<Cid> = shared_memory_cids
            .iter()
            .filter(|(id, _)| group_shared_block_ids.contains(id))
            .map(|(_, cid)| *cid)
            .collect();

        group_export_thins.push(pattern_core::export::GroupExportThin {
            group: new_group_record,
            members,
            agent_cids,
            shared_memory_cids: group_shared_cids,
            shared_attachment_exports: group_shared_attachments,
        });
    }

    // Standalone agents (those not in any group)
    let grouped_agent_ids: std::collections::HashSet<_> = old_export
        .groups
        .iter()
        .flat_map(|g| g.member_agent_cids.iter().map(|(id, _)| id.to_string()))
        .collect();

    let standalone_agent_cids: Vec<Cid> = agent_exports
        .iter()
        .filter(|(id, _)| !grouped_agent_ids.contains(*id))
        .map(|(_, cid)| *cid)
        .collect();

    // Create the v3 ConstellationExport
    let new_export = pattern_core::export::ConstellationExport {
        version: EXPORT_VERSION,
        owner_id: old_export.constellation.owner_id.to_string(),
        exported_at: Utc::now(),
        agent_exports,
        group_exports: group_export_thins,
        standalone_agent_cids,
        all_memory_block_cids: shared_memory_cids.into_iter().map(|(_, cid)| cid).collect(),
        shared_attachments,
    };

    let (export_cid, export_data) = encode_block(&new_export, "ConstellationExport")
        .map_err(|e| ConversionError::Core(e.to_string()))?;
    new_blocks.push((export_cid, export_data));

    Ok((
        export_cid,
        new_blocks,
        (
            agent_count,
            group_count,
            message_count,
            memory_count,
            archival_count,
        ),
    ))
}

/// Convert a message chunk from v1/v2 to v3.
/// May produce multiple chunks if the old chunk exceeds size limits.
fn convert_message_chunk(
    old_chunk: &OldMessageChunk,
    agent_id: &str,
    base_chunk_index: u32,
) -> Result<(Vec<Cid>, Vec<(Cid, Vec<u8>)>, u64, u32), ConversionError> {
    use pattern_core::export::estimate_size;

    let messages: Vec<MessageExport> = old_chunk
        .messages
        .iter()
        .map(|(msg, relation)| convert_message(msg, relation, agent_id))
        .collect();

    let total_message_count = messages.len() as u64;

    // Try to fit messages into chunks under TARGET_CHUNK_BYTES
    let mut chunks: Vec<MessageChunk> = Vec::new();
    let mut current_messages: Vec<MessageExport> = Vec::new();
    let mut current_size: usize = 200; // Base overhead for chunk structure
    let mut chunk_index = base_chunk_index;

    for msg in messages {
        let msg_size = estimate_size(&msg).unwrap_or(1000);

        // If adding this message would exceed target, flush current chunk
        if !current_messages.is_empty() && current_size + msg_size > TARGET_CHUNK_BYTES {
            let start_pos = current_messages
                .first()
                .map(|m| m.position.clone())
                .unwrap_or_default();
            let end_pos = current_messages
                .last()
                .map(|m| m.position.clone())
                .unwrap_or_default();

            chunks.push(MessageChunk {
                chunk_index,
                start_position: start_pos,
                end_position: end_pos,
                message_count: current_messages.len() as u32,
                messages: std::mem::take(&mut current_messages),
            });
            chunk_index += 1;
            current_size = 200;
        }

        current_size += msg_size;
        current_messages.push(msg);
    }

    // Flush remaining messages
    if !current_messages.is_empty() {
        let start_pos = current_messages
            .first()
            .map(|m| m.position.clone())
            .unwrap_or_default();
        let end_pos = current_messages
            .last()
            .map(|m| m.position.clone())
            .unwrap_or_default();

        chunks.push(MessageChunk {
            chunk_index,
            start_position: start_pos,
            end_position: end_pos,
            message_count: current_messages.len() as u32,
            messages: current_messages,
        });
        chunk_index += 1;
    }

    // Encode all chunks
    let mut cids: Vec<Cid> = Vec::new();
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();

    for chunk in chunks {
        let (cid, data) = encode_block(&chunk, "MessageChunk")
            .map_err(|e| ConversionError::Core(e.to_string()))?;
        cids.push(cid);
        blocks.push((cid, data));
    }

    let chunks_created = chunk_index - base_chunk_index;
    Ok((cids, blocks, total_message_count, chunks_created))
}

/// Collected relation data from memory chunk conversion.
/// Used to build SharedBlockAttachmentExport records for group/constellation exports.
#[derive(Debug, Clone)]
struct CollectedMemoryRelation {
    block_id: String,
    agent_id: String,
    permission: MemoryPermission,
    created_at: chrono::DateTime<Utc>,
}

/// Result of converting a memory chunk.
struct MemoryChunkConversionResult {
    /// CIDs of memory blocks (core/working only)
    memory_block_cids: Vec<Cid>,
    /// CIDs of archival entries (converted from archival blocks)
    archival_entry_cids: Vec<Cid>,
    /// All encoded blocks
    blocks: Vec<(Cid, Vec<u8>)>,
    /// Count of memory blocks
    memory_count: u64,
    /// Count of archival entries
    archival_count: u64,
    /// Relations for shared block tracking
    relations: Vec<CollectedMemoryRelation>,
}

/// Convert a memory chunk from v1/v2 to v3.
/// Archival blocks are converted to ArchivalEntryExport (if `archival_blocks_to_entries` is true),
/// others to MemoryBlockExport.
fn convert_memory_chunk(
    old_chunk: &OldMemoryChunk,
    agent_id: &str,
    options: &ConversionOptions,
) -> Result<MemoryChunkConversionResult, ConversionError> {
    let mut memory_block_cids: Vec<Cid> = Vec::new();
    let mut archival_entry_cids: Vec<Cid> = Vec::new();
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut relations: Vec<CollectedMemoryRelation> = Vec::new();
    let mut memory_count = 0u64;
    let mut archival_count = 0u64;

    for (old_block, relation) in &old_chunk.memories {
        if old_block.memory_type == MemoryType::Archival && options.archival_blocks_to_entries {
            // Convert to archival entry (searchable text)
            let entry = ArchivalEntryExport {
                id: old_block.id.to_string(),
                agent_id: agent_id.to_string(),
                content: old_block.value.clone(),
                metadata: Some(old_block.metadata.clone()),
                chunk_index: 0,
                parent_entry_id: None,
                created_at: old_block.created_at,
            };
            let (cid, data) = encode_block(&entry, "ArchivalEntryExport")
                .map_err(|e| ConversionError::Core(e.to_string()))?;
            archival_entry_cids.push(cid);
            blocks.push((cid, data));
            archival_count += 1;
        } else {
            // Convert to memory block
            let (block_export, snapshot_blocks) = convert_memory_block(old_block, agent_id)?;
            blocks.extend(snapshot_blocks);

            let (cid, data) = encode_block(&block_export, "MemoryBlockExport")
                .map_err(|e| ConversionError::Core(e.to_string()))?;
            memory_block_cids.push(cid);
            blocks.push((cid, data));
            memory_count += 1;

            // Only collect relation data for blocks that become MemoryBlockExport
            // (not archival entries, which can't be "shared" in the same way)
            relations.push(CollectedMemoryRelation {
                block_id: old_block.id.to_string(),
                agent_id: relation.in_id.to_string(),
                permission: convert_permission(&relation.access_level),
                created_at: relation.created_at,
            });
        }
    }

    Ok(MemoryChunkConversionResult {
        memory_block_cids,
        archival_entry_cids,
        blocks,
        memory_count,
        archival_count,
        relations,
    })
}

/// Convert an old agent record to v3 AgentRecord.
fn convert_agent_record(old: &OldAgentRecord) -> Result<AgentRecord, ConversionError> {
    Ok(AgentRecord {
        id: old.id.to_string(),
        name: old.name.clone(),
        description: None,
        model_provider: old
            .model_id
            .as_ref()
            .and_then(|id| id.split('/').next())
            .unwrap_or("anthropic")
            .to_string(),
        model_name: old
            .model_id
            .as_ref()
            .map(|id| id.split('/').last().unwrap_or(id).to_string())
            .unwrap_or_else(|| "claude-3-5-sonnet".to_string()),
        system_prompt: old.base_instructions.clone(),
        config: serde_json::json!({
            "max_messages": old.max_messages,
            "max_message_age_hours": old.max_message_age_hours,
            "compression_threshold": old.compression_threshold,
            "memory_char_limit": old.memory_char_limit,
            "enable_thinking": old.enable_thinking,
        }),
        enabled_tools: Vec::new(), // Old format stored tools differently
        tool_rules: if old.tool_rules.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&old.tool_rules).unwrap_or_default())
        },
        status: AgentStatus::Active,
        created_at: old.created_at,
        updated_at: old.updated_at,
    })
}

/// Convert an old group to v3 GroupRecord.
fn convert_group_record(old: &AgentGroup) -> Result<GroupRecord, ConversionError> {
    Ok(GroupRecord {
        id: old.id.to_string(),
        name: old.name.clone(),
        description: Some(old.description.clone()),
        pattern_type: convert_pattern_type(&old.coordination_pattern),
        pattern_config: serde_json::to_value(&old.coordination_pattern).unwrap_or_default(),
        created_at: old.created_at,
        updated_at: old.updated_at,
    })
}

/// Convert an old message to v3 MessageExport.
fn convert_message(
    old: &OldMessage,
    relation: &AgentMessageRelation,
    agent_id: &str,
) -> MessageExport {
    MessageExport {
        id: old.id.to_string(),
        agent_id: agent_id.to_string(),
        position: relation
            .position
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| old.id.to_string()),
        batch_id: old.batch.as_ref().map(|b| b.to_string()),
        sequence_in_batch: old.sequence_num.map(|n| n as i64),
        role: convert_role(&old.role),
        content_json: serde_json::to_value(&old.content).unwrap_or_default(),
        content_preview: old.content.text().map(|s| s.to_string()),
        batch_type: old.batch_type.map(|bt| convert_batch_type(&bt)),
        source: None,
        source_metadata: None,
        is_archived: matches!(relation.message_type, MessageRelationType::Archived),
        is_deleted: false,
        created_at: old.created_at,
    }
}

/// Convert an old memory block to v3 MemoryBlockExport.
fn convert_memory_block(
    old: &OldMemoryBlock,
    agent_id: &str,
) -> Result<(MemoryBlockExport, Vec<(Cid, Vec<u8>)>), ConversionError> {
    // Create Loro snapshot from the text content
    let loro_snapshot = text_to_loro_snapshot(&old.value);
    let total_snapshot_bytes = loro_snapshot.len() as u64;

    // Chunk the snapshot if needed
    let mut snapshot_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut snapshot_chunk_cids: Vec<Cid> = Vec::new();

    if loro_snapshot.len() <= TARGET_CHUNK_BYTES {
        // Single chunk
        let chunk = SnapshotChunk {
            index: 0,
            data: loro_snapshot,
            next_cid: None,
        };
        let (cid, data) = encode_block(&chunk, "SnapshotChunk")
            .map_err(|e| ConversionError::Core(e.to_string()))?;
        snapshot_chunk_cids.push(cid);
        snapshot_blocks.push((cid, data));
    } else {
        // Multiple chunks - create linked list
        let chunks: Vec<Vec<u8>> = loro_snapshot
            .chunks(TARGET_CHUNK_BYTES)
            .map(|c| c.to_vec())
            .collect();

        // Process in reverse to build the linked list
        let mut next_cid: Option<Cid> = None;
        for (idx, chunk_data) in chunks.iter().enumerate().rev() {
            let chunk = SnapshotChunk {
                index: idx as u32,
                data: chunk_data.clone(),
                next_cid,
            };
            let (cid, data) = encode_block(&chunk, "SnapshotChunk")
                .map_err(|e| ConversionError::Core(e.to_string()))?;
            snapshot_chunk_cids.insert(0, cid);
            snapshot_blocks.insert(0, (cid, data));
            next_cid = Some(cid);
        }
    }

    let export = MemoryBlockExport {
        id: old.id.to_string(),
        agent_id: agent_id.to_string(),
        label: old.label.to_string(),
        description: old.description.clone().unwrap_or_default(),
        block_type: convert_block_type(&old.memory_type),
        char_limit: 5000, // Default
        permission: convert_permission(&old.permission),
        pinned: old.pinned,
        content_preview: Some(old.value.clone()),
        metadata: Some(old.metadata.clone()),
        is_active: old.is_active,
        frontier: None, // New Loro document
        last_seq: 0,
        created_at: old.created_at,
        updated_at: old.updated_at,
        snapshot_chunk_cids,
        total_snapshot_bytes,
    };

    Ok((export, snapshot_blocks))
}

/// Build SharedBlockAttachmentExport records from collected memory relations.
///
/// A block is considered "shared" if it appears in relations for multiple agents.
/// For each shared block, the first agent encountered is treated as the "owner"
/// and subsequent agents get SharedBlockAttachmentExport records.
///
/// Returns (Vec<(block_id, Cid)>, Vec<SharedBlockAttachmentExport>)
fn build_shared_block_exports(
    relations: &[CollectedMemoryRelation],
    encoded_blocks: &[(Cid, Vec<u8>)],
) -> Result<(Vec<(String, Cid)>, Vec<SharedBlockAttachmentExport>), ConversionError> {
    use std::collections::{HashMap, HashSet};

    // Group relations by block_id
    let mut block_agents: HashMap<String, Vec<&CollectedMemoryRelation>> = HashMap::new();
    for rel in relations {
        block_agents
            .entry(rel.block_id.clone())
            .or_default()
            .push(rel);
    }

    // Find blocks that have multiple agents (shared blocks)
    let mut shared_block_cids: Vec<(String, Cid)> = Vec::new();
    let mut shared_attachments: Vec<SharedBlockAttachmentExport> = Vec::new();
    let mut seen_block_ids: HashSet<String> = HashSet::new();

    for (block_id, agent_relations) in &block_agents {
        // Collect unique agent IDs for this block
        let unique_agents: Vec<_> = {
            let mut seen = HashSet::new();
            agent_relations
                .iter()
                .filter(|r| seen.insert(r.agent_id.clone()))
                .collect()
        };

        if unique_agents.len() > 1 {
            // This is a shared block - first agent is "owner", rest get attachments
            let owner_agent = &unique_agents[0].agent_id;

            // Find the CID for this block in the encoded blocks
            // We need to search through encoded blocks to find the MemoryBlockExport with this ID
            let block_cid = find_memory_block_cid(block_id, encoded_blocks)?;

            if !seen_block_ids.contains(block_id) {
                shared_block_cids.push((block_id.clone(), block_cid));
                seen_block_ids.insert(block_id.clone());
            }

            // Create attachments for non-owner agents
            for rel in unique_agents.iter().skip(1) {
                if rel.agent_id != *owner_agent {
                    shared_attachments.push(SharedBlockAttachmentExport {
                        block_id: block_id.clone(),
                        agent_id: rel.agent_id.clone(),
                        permission: rel.permission,
                        attached_at: rel.created_at,
                    });
                }
            }
        }
    }

    Ok((shared_block_cids, shared_attachments))
}

/// Find the CID of a MemoryBlockExport in the encoded blocks by its ID.
fn find_memory_block_cid(
    block_id: &str,
    encoded_blocks: &[(Cid, Vec<u8>)],
) -> Result<Cid, ConversionError> {
    for (cid, data) in encoded_blocks {
        // Try to decode as MemoryBlockExport
        if let Ok(export) = from_slice::<MemoryBlockExport>(data) {
            if export.id == block_id {
                return Ok(*cid);
            }
        }
    }
    Err(ConversionError::CidNotFound(format!(
        "MemoryBlockExport with id {}",
        block_id
    )))
}

// =============================================================================
// Type Conversion Helpers
// =============================================================================

/// Convert ChatRole to MessageRole.
fn convert_role(old: &ChatRole) -> MessageRole {
    match old {
        ChatRole::System => MessageRole::System,
        ChatRole::User => MessageRole::User,
        ChatRole::Assistant => MessageRole::Assistant,
        ChatRole::Tool => MessageRole::Tool,
    }
}

/// Convert old MemoryType to MemoryBlockType.
fn convert_block_type(old: &MemoryType) -> MemoryBlockType {
    match old {
        MemoryType::Core => MemoryBlockType::Core,
        MemoryType::Working => MemoryBlockType::Working,
        MemoryType::Archival => MemoryBlockType::Archival,
    }
}

/// Convert old MemoryPermission to new MemoryPermission.
fn convert_permission(old: &OldPermission) -> MemoryPermission {
    match old {
        OldPermission::ReadOnly => MemoryPermission::ReadOnly,
        OldPermission::Partner => MemoryPermission::Partner,
        OldPermission::Human => MemoryPermission::Human,
        OldPermission::Append => MemoryPermission::Append,
        OldPermission::ReadWrite => MemoryPermission::ReadWrite,
        OldPermission::Admin => MemoryPermission::Admin,
    }
}

/// Convert old BatchType to new BatchType.
fn convert_batch_type(old: &OldBatchType) -> BatchType {
    match old {
        OldBatchType::UserRequest => BatchType::UserRequest,
        OldBatchType::AgentToAgent => BatchType::AgentToAgent,
        OldBatchType::SystemTrigger => BatchType::SystemTrigger,
        OldBatchType::Continuation => BatchType::Continuation,
    }
}

/// Convert old CoordinationPattern to PatternType.
fn convert_pattern_type(old: &CoordinationPattern) -> PatternType {
    match old {
        CoordinationPattern::Supervisor { .. } => PatternType::Supervisor,
        CoordinationPattern::RoundRobin { .. } => PatternType::RoundRobin,
        CoordinationPattern::Voting { .. } => PatternType::Voting,
        CoordinationPattern::Pipeline { .. } => PatternType::Pipeline,
        CoordinationPattern::Dynamic { .. } => PatternType::Dynamic,
        CoordinationPattern::Sleeptime { .. } => PatternType::Sleeptime,
    }
}

/// Convert old GroupMemberRole to new GroupMemberRole.
fn convert_group_member_role(old: &crate::groups::GroupMemberRole) -> Option<GroupMemberRole> {
    use crate::groups::GroupMemberRole as OldRole;
    Some(match old {
        OldRole::Regular => GroupMemberRole::Regular,
        OldRole::Supervisor => GroupMemberRole::Supervisor,
        OldRole::Specialist { domain } => GroupMemberRole::Specialist {
            domain: domain.clone(),
        },
    })
}

/// Convert plain text to a Loro document snapshot.
fn text_to_loro_snapshot(text: &str) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    let text_container = doc.get_text("content");
    text_container.insert(0, text).unwrap();
    doc.export(loro::ExportMode::Snapshot).unwrap_or_default()
}

/// Split a legacy message_summary into individual ArchiveSummaryExport records.
///
/// The old format stored all summaries in a single string, separated by 2+ newlines.
/// We split them and chain via previous_summary_id, with depth=1 for all.
fn convert_message_summary(
    summary: &str,
    agent_id: &str,
    _first_message_pos: Option<&str>,
    _last_message_pos: Option<&str>,
) -> Result<(Vec<Cid>, Vec<(Cid, Vec<u8>)>), ConversionError> {
    use regex::Regex;

    let delim_re = Regex::new(r"\n{2,}").expect("valid delimiter regex");
    let blocks: Vec<&str> = delim_re
        .split(summary)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if blocks.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut cids: Vec<Cid> = Vec::new();
    let mut encoded_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut previous_id: Option<String> = None;
    let now = Utc::now();

    for (idx, block_text) in blocks.iter().enumerate() {
        let summary_id = format!("sum_{agent_id}_{idx}");

        let export = ArchiveSummaryExport {
            id: summary_id.clone(),
            agent_id: agent_id.to_string(),
            summary: (*block_text).to_string(),
            start_position: format!("legacy_{idx}_start"),
            end_position: format!("legacy_{idx}_end"),
            message_count: 0, // Unknown from old format
            previous_summary_id: previous_id.clone(),
            depth: 1,
            created_at: now,
        };

        let (cid, data) = encode_block(&export, "ArchiveSummaryExport")
            .map_err(|e| ConversionError::Core(e.to_string()))?;

        cids.push(cid);
        encoded_blocks.push((cid, data));
        previous_id = Some(summary_id);
    }

    Ok((cids, encoded_blocks))
}

// =============================================================================
// CAR File Writing
// =============================================================================

/// Write a CAR file with the given root and blocks.
async fn write_car_file(
    path: &Path,
    root_cid: Cid,
    root_data: Vec<u8>,
    blocks: Vec<(Cid, Vec<u8>)>,
) -> Result<(), ConversionError> {
    use iroh_car::{CarHeader, CarWriter};

    let file = File::create(path).await?;

    // Create CAR writer with header
    let header = CarHeader::new_v1(vec![root_cid]);
    let mut writer = CarWriter::new(header, file);

    // Write root block first
    writer
        .write(root_cid, &root_data)
        .await
        .map_err(|e| ConversionError::CarRead(e.to_string()))?;

    // Write all other blocks
    for (cid, data) in blocks {
        writer
            .write(cid, &data)
            .await
            .map_err(|e| ConversionError::CarRead(e.to_string()))?;
    }

    writer
        .finish()
        .await
        .map_err(|e| ConversionError::CarRead(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_role() {
        assert!(matches!(
            convert_role(&ChatRole::System),
            MessageRole::System
        ));
        assert!(matches!(convert_role(&ChatRole::User), MessageRole::User));
        assert!(matches!(
            convert_role(&ChatRole::Assistant),
            MessageRole::Assistant
        ));
        assert!(matches!(convert_role(&ChatRole::Tool), MessageRole::Tool));
    }

    #[test]
    fn test_convert_block_type() {
        assert!(matches!(
            convert_block_type(&MemoryType::Core),
            MemoryBlockType::Core
        ));
        assert!(matches!(
            convert_block_type(&MemoryType::Working),
            MemoryBlockType::Working
        ));
        assert!(matches!(
            convert_block_type(&MemoryType::Archival),
            MemoryBlockType::Archival
        ));
    }

    #[test]
    fn test_text_to_loro_snapshot() {
        let snapshot = text_to_loro_snapshot("Hello, world!");
        assert!(!snapshot.is_empty());

        // Verify we can reconstruct the text
        let doc = loro::LoroDoc::new();
        doc.import(&snapshot).unwrap();
        let text = doc.get_text("content");
        assert_eq!(text.to_string(), "Hello, world!");
    }
}
