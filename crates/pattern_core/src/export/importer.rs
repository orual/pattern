//! CAR archive importer for Pattern agents, groups, and constellations.
//!
//! This module provides the inverse of the exporter, allowing CAR archives
//! to be imported back into a Pattern database.

use std::collections::HashMap;

use chrono::Utc;
use cid::Cid;
use iroh_car::CarReader;
use serde_ipld_dagcbor::from_slice as decode_dag_cbor;
use sqlx::SqlitePool;
use sqlx::types::Json;
use tokio::io::AsyncRead;

use pattern_db::models::{
    Agent, AgentGroup, ArchivalEntry, ArchiveSummary, GroupMember, MemoryBlock, Message,
};
use pattern_db::queries;

use super::{
    EXPORT_VERSION,
    types::{
        AgentExport, ArchivalEntryExport, ArchiveSummaryExport, ConstellationExport,
        ExportManifest, ExportType, GroupConfigExport, GroupExport, GroupExportThin,
        GroupMemberExport, GroupRecord, ImportOptions, MemoryBlockExport, MessageChunk,
        MessageExport, SharedBlockAttachmentExport, SnapshotChunk,
    },
};
use crate::error::{CoreError, Result};

/// Result of an import operation.
#[derive(Debug, Clone, Default)]
pub struct ImportResult {
    /// IDs of imported agents
    pub agent_ids: Vec<String>,

    /// IDs of imported groups
    pub group_ids: Vec<String>,

    /// Number of messages imported
    pub message_count: u64,

    /// Number of memory blocks imported
    pub memory_block_count: u64,

    /// Number of archival entries imported
    pub archival_entry_count: u64,

    /// Number of archive summaries imported
    pub archive_summary_count: u64,
}

impl ImportResult {
    /// Merge another result into this one.
    fn merge(&mut self, other: ImportResult) {
        self.agent_ids.extend(other.agent_ids);
        self.group_ids.extend(other.group_ids);
        self.message_count += other.message_count;
        self.memory_block_count += other.memory_block_count;
        self.archival_entry_count += other.archival_entry_count;
        self.archive_summary_count += other.archive_summary_count;
    }
}

/// CAR archive importer.
pub struct Importer {
    pool: SqlitePool,
}

impl Importer {
    /// Create a new importer with the given database pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Import a CAR archive from the given reader.
    ///
    /// Reads the CAR file, validates the manifest, and dispatches to the
    /// appropriate import function based on export type.
    pub async fn import<R: AsyncRead + Unpin + Send>(
        &self,
        input: R,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        // Read all blocks from CAR file into memory
        let (root_cids, blocks) = self.read_car(input).await?;

        // We expect exactly one root CID (the manifest)
        let root_cid = root_cids.first().ok_or_else(|| CoreError::ExportError {
            operation: "reading CAR".to_string(),
            cause: "CAR file has no root CID".to_string(),
        })?;

        // Load and parse manifest
        let manifest_bytes = blocks.get(root_cid).ok_or_else(|| CoreError::ExportError {
            operation: "reading manifest".to_string(),
            cause: "Root CID block not found in CAR".to_string(),
        })?;

        let manifest: ExportManifest =
            decode_dag_cbor(manifest_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "ExportManifest".to_string(),
                details: e.to_string(),
            })?;

        // Validate version - reject v1 and v2
        if manifest.version < 3 {
            return Err(CoreError::ExportError {
                operation: "version check".to_string(),
                cause: format!(
                    "CAR export version {} is not supported. This importer requires version 3 or later. \
                     Please re-export using the current version of Pattern.",
                    manifest.version
                ),
            });
        }

        // Ensure version is not newer than what we support
        if manifest.version > EXPORT_VERSION {
            return Err(CoreError::ExportError {
                operation: "version check".to_string(),
                cause: format!(
                    "CAR export version {} is newer than supported version {}. \
                     Please update Pattern to import this file.",
                    manifest.version, EXPORT_VERSION
                ),
            });
        }

        // Dispatch based on export type
        match manifest.export_type {
            ExportType::Agent => {
                self.import_agent_from_cid(&manifest.data_cid, &blocks, options)
                    .await
            }
            ExportType::Group => {
                self.import_group_from_cid(&manifest.data_cid, &blocks, options)
                    .await
            }
            ExportType::Constellation => {
                self.import_constellation_from_cid(&manifest.data_cid, &blocks, options)
                    .await
            }
        }
    }

    /// Read all blocks from a CAR file into memory.
    async fn read_car<R: AsyncRead + Unpin + Send>(
        &self,
        input: R,
    ) -> Result<(Vec<Cid>, HashMap<Cid, Vec<u8>>)> {
        let mut reader = CarReader::new(input)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "opening CAR".to_string(),
                cause: e,
            })?;

        let root_cids = reader.header().roots().to_vec();
        let mut blocks = HashMap::new();

        loop {
            match reader.next_block().await {
                Ok(Some((cid, data))) => {
                    blocks.insert(cid, data);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(CoreError::CarError {
                        operation: "reading block".to_string(),
                        cause: e,
                    });
                }
            }
        }

        Ok((root_cids, blocks))
    }

    /// Import an agent from a CID reference.
    async fn import_agent_from_cid(
        &self,
        data_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let data_bytes = blocks.get(data_cid).ok_or_else(|| CoreError::ExportError {
            operation: "reading agent export".to_string(),
            cause: format!("Agent export block {} not found", data_cid),
        })?;

        let agent_export: AgentExport =
            decode_dag_cbor(data_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "AgentExport".to_string(),
                details: e.to_string(),
            })?;

        self.import_agent(&agent_export, blocks, options, None)
            .await
    }

    /// Import an agent and all its data.
    ///
    /// If `id_override` is provided, use it instead of the original ID.
    /// This is used for deduplication in constellation imports.
    async fn import_agent(
        &self,
        export: &AgentExport,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
        id_override: Option<&str>,
    ) -> Result<ImportResult> {
        let mut result = ImportResult::default();

        // Determine the agent ID to use
        let agent_id = if options.preserve_ids {
            export.agent.id.clone()
        } else if let Some(override_id) = id_override {
            override_id.to_string()
        } else {
            generate_id()
        };

        // Determine the agent name
        let agent_name = options
            .rename
            .clone()
            .unwrap_or_else(|| export.agent.name.clone());

        // Create the agent record
        let now = Utc::now();
        let agent = Agent {
            id: agent_id.clone(),
            name: agent_name,
            description: export.agent.description.clone(),
            model_provider: export.agent.model_provider.clone(),
            model_name: export.agent.model_name.clone(),
            system_prompt: export.agent.system_prompt.clone(),
            config: Json(export.agent.config.clone()),
            enabled_tools: Json(export.agent.enabled_tools.clone()),
            tool_rules: export.agent.tool_rules.clone().map(Json),
            status: export.agent.status,
            created_at: now,
            updated_at: now,
        };

        queries::create_agent(&self.pool, &agent).await?;
        result.agent_ids.push(agent_id.clone());

        // Import memory blocks
        for block_cid in &export.memory_block_cids {
            self.import_memory_block(block_cid, blocks, &agent_id, options)
                .await?;
            result.memory_block_count += 1;
        }

        // Import messages if requested
        if options.include_messages {
            // Maintain batch ID mapping across all message chunks for this agent
            let mut batch_id_map: HashMap<String, String> = HashMap::new();
            for chunk_cid in &export.message_chunk_cids {
                let count = self
                    .import_message_chunk(chunk_cid, blocks, &agent_id, options, &mut batch_id_map)
                    .await?;
                result.message_count += count;
            }
        }

        // Import archival entries if requested
        if options.include_archival {
            for entry_cid in &export.archival_entry_cids {
                self.import_archival_entry(entry_cid, blocks, &agent_id, options)
                    .await?;
                result.archival_entry_count += 1;
            }
        }

        // Import archive summaries
        for summary_cid in &export.archive_summary_cids {
            self.import_archive_summary(summary_cid, blocks, &agent_id, options)
                .await?;
            result.archive_summary_count += 1;
        }

        Ok(result)
    }

    /// Import a memory block from a CID reference.
    async fn import_memory_block(
        &self,
        block_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        agent_id: &str,
        options: &ImportOptions,
    ) -> Result<()> {
        let block_bytes = blocks
            .get(block_cid)
            .ok_or_else(|| CoreError::ExportError {
                operation: "reading memory block".to_string(),
                cause: format!("Memory block {} not found", block_cid),
            })?;

        let export: MemoryBlockExport =
            decode_dag_cbor(block_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "MemoryBlockExport".to_string(),
                details: e.to_string(),
            })?;

        // Reconstruct the Loro snapshot from chunks
        let loro_snapshot = self.reconstruct_snapshot(&export.snapshot_chunk_cids, blocks)?;

        // Determine the block ID
        let block_id = if options.preserve_ids {
            export.id.clone()
        } else {
            generate_id()
        };

        let now = Utc::now();
        let memory_block = MemoryBlock {
            id: block_id,
            agent_id: agent_id.to_string(),
            label: export.label.clone(),
            description: export.description.clone(),
            block_type: export.block_type,
            char_limit: export.char_limit,
            permission: export.permission,
            pinned: export.pinned,
            loro_snapshot,
            content_preview: export.content_preview.clone(),
            metadata: export.metadata.clone().map(Json),
            embedding_model: None, // Embeddings are not exported
            is_active: export.is_active,
            frontier: export.frontier.clone(),
            last_seq: export.last_seq,
            created_at: now,
            updated_at: now,
        };

        queries::create_block(&self.pool, &memory_block).await?;
        Ok(())
    }

    /// Reconstruct a Loro snapshot from chunk CIDs.
    fn reconstruct_snapshot(
        &self,
        chunk_cids: &[Cid],
        blocks: &HashMap<Cid, Vec<u8>>,
    ) -> Result<Vec<u8>> {
        if chunk_cids.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();

        for cid in chunk_cids {
            let chunk_bytes = blocks.get(cid).ok_or_else(|| CoreError::ExportError {
                operation: "reading snapshot chunk".to_string(),
                cause: format!("Snapshot chunk {} not found", cid),
            })?;

            let chunk: SnapshotChunk =
                decode_dag_cbor(chunk_bytes).map_err(|e| CoreError::DagCborDecodingError {
                    data_type: "SnapshotChunk".to_string(),
                    details: e.to_string(),
                })?;

            result.extend_from_slice(&chunk.data);
        }

        Ok(result)
    }

    /// Import a message chunk from a CID reference.
    ///
    /// Uses a batch ID map to ensure messages with the same original batch_id
    /// get the same new batch_id.
    async fn import_message_chunk(
        &self,
        chunk_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        agent_id: &str,
        options: &ImportOptions,
        batch_id_map: &mut HashMap<String, String>,
    ) -> Result<u64> {
        let chunk_bytes = blocks
            .get(chunk_cid)
            .ok_or_else(|| CoreError::ExportError {
                operation: "reading message chunk".to_string(),
                cause: format!("Message chunk {} not found", chunk_cid),
            })?;

        let chunk: MessageChunk =
            decode_dag_cbor(chunk_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "MessageChunk".to_string(),
                details: e.to_string(),
            })?;

        let mut count = 0;
        for msg_export in &chunk.messages {
            self.import_message(msg_export, agent_id, options, batch_id_map)
                .await?;
            count += 1;
        }

        Ok(count)
    }

    /// Import a single message.
    ///
    /// Uses a batch ID map to maintain consistency across messages in the same batch.
    async fn import_message(
        &self,
        export: &MessageExport,
        agent_id: &str,
        options: &ImportOptions,
        batch_id_map: &mut HashMap<String, String>,
    ) -> Result<()> {
        // Determine the message ID
        let msg_id = if options.preserve_ids {
            export.id.clone()
        } else {
            generate_id()
        };

        // Batch ID handling - maintain mapping for consistency
        let batch_id = if options.preserve_ids {
            export.batch_id.clone()
        } else if let Some(old_batch_id) = &export.batch_id {
            // Look up or create a new batch ID for this old batch ID
            let new_batch_id = batch_id_map
                .entry(old_batch_id.clone())
                .or_insert_with(generate_id)
                .clone();
            Some(new_batch_id)
        } else {
            None
        };

        let message = Message {
            id: msg_id,
            agent_id: agent_id.to_string(),
            position: export.position.clone(),
            batch_id,
            sequence_in_batch: export.sequence_in_batch,
            role: export.role,
            content_json: Json(export.content_json.clone()),
            content_preview: export.content_preview.clone(),
            batch_type: export.batch_type,
            source: export.source.clone(),
            source_metadata: export.source_metadata.clone().map(Json),
            is_archived: export.is_archived,
            is_deleted: export.is_deleted,
            created_at: export.created_at,
        };

        queries::create_message(&self.pool, &message).await?;
        Ok(())
    }

    /// Import an archival entry from a CID reference.
    async fn import_archival_entry(
        &self,
        entry_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        agent_id: &str,
        options: &ImportOptions,
    ) -> Result<()> {
        let entry_bytes = blocks
            .get(entry_cid)
            .ok_or_else(|| CoreError::ExportError {
                operation: "reading archival entry".to_string(),
                cause: format!("Archival entry {} not found", entry_cid),
            })?;

        let export: ArchivalEntryExport =
            decode_dag_cbor(entry_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "ArchivalEntryExport".to_string(),
                details: e.to_string(),
            })?;

        // Determine the entry ID
        let entry_id = if options.preserve_ids {
            export.id.clone()
        } else {
            generate_id()
        };

        // Handle parent entry ID - keep if preserving, otherwise set to None
        // (parent linking would require a two-pass import)
        let parent_entry_id = if options.preserve_ids {
            export.parent_entry_id.clone()
        } else {
            None
        };

        let entry = ArchivalEntry {
            id: entry_id,
            agent_id: agent_id.to_string(),
            content: export.content.clone(),
            metadata: export.metadata.clone().map(Json),
            chunk_index: export.chunk_index,
            parent_entry_id,
            created_at: export.created_at,
        };

        queries::create_archival_entry(&self.pool, &entry).await?;
        Ok(())
    }

    /// Import an archive summary from a CID reference.
    async fn import_archive_summary(
        &self,
        summary_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        agent_id: &str,
        options: &ImportOptions,
    ) -> Result<()> {
        let summary_bytes = blocks
            .get(summary_cid)
            .ok_or_else(|| CoreError::ExportError {
                operation: "reading archive summary".to_string(),
                cause: format!("Archive summary {} not found", summary_cid),
            })?;

        let export: ArchiveSummaryExport =
            decode_dag_cbor(summary_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "ArchiveSummaryExport".to_string(),
                details: e.to_string(),
            })?;

        // Determine the summary ID
        let summary_id = if options.preserve_ids {
            export.id.clone()
        } else {
            generate_id()
        };

        // Handle previous summary ID - keep if preserving, otherwise set to None
        let previous_summary_id = if options.preserve_ids {
            export.previous_summary_id.clone()
        } else {
            None
        };

        let summary = ArchiveSummary {
            id: summary_id,
            agent_id: agent_id.to_string(),
            summary: export.summary.clone(),
            start_position: export.start_position.clone(),
            end_position: export.end_position.clone(),
            message_count: export.message_count,
            previous_summary_id,
            depth: export.depth,
            created_at: export.created_at,
        };

        queries::create_archive_summary(&self.pool, &summary).await?;
        Ok(())
    }

    /// Import a group from a CID reference.
    ///
    /// Handles both thin (GroupConfigExport) and full (GroupExport) variants.
    async fn import_group_from_cid(
        &self,
        data_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let data_bytes = blocks.get(data_cid).ok_or_else(|| CoreError::ExportError {
            operation: "reading group export".to_string(),
            cause: format!("Group export block {} not found", data_cid),
        })?;

        // Try to decode as full GroupExport first
        if let Ok(group_export) = decode_dag_cbor::<GroupExport>(data_bytes) {
            return self.import_group_full(&group_export, blocks, options).await;
        }

        // Try thin GroupConfigExport
        if let Ok(config_export) = decode_dag_cbor::<GroupConfigExport>(data_bytes) {
            return self.import_group_thin(&config_export, options).await;
        }

        Err(CoreError::DagCborDecodingError {
            data_type: "GroupExport or GroupConfigExport".to_string(),
            details: "Failed to decode as either full or thin group export".to_string(),
        })
    }

    /// Import a full group with inline agent exports.
    async fn import_group_full(
        &self,
        export: &GroupExport,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let mut result = ImportResult::default();

        // Map old agent IDs to new agent IDs
        let mut agent_id_map: HashMap<String, String> = HashMap::new();

        // Import all agents first
        for agent_export in &export.agent_exports {
            let new_id = if options.preserve_ids {
                agent_export.agent.id.clone()
            } else {
                generate_id()
            };

            agent_id_map.insert(agent_export.agent.id.clone(), new_id.clone());

            // Don't use rename for group members - only applies to top-level export
            let agent_options = ImportOptions {
                owner_id: options.owner_id.clone(),
                rename: None, // Don't rename individual agents in a group
                preserve_ids: options.preserve_ids,
                include_messages: options.include_messages,
                include_archival: options.include_archival,
            };

            let agent_result = self
                .import_agent(agent_export, blocks, &agent_options, Some(&new_id))
                .await?;
            result.merge(agent_result);
        }

        // Create the group
        let group_id = if options.preserve_ids {
            export.group.id.clone()
        } else {
            generate_id()
        };

        let group_name = options
            .rename
            .clone()
            .unwrap_or_else(|| export.group.name.clone());

        let group = self.create_group_from_record(&export.group, &group_id, &group_name)?;
        queries::create_group(&self.pool, &group).await?;
        result.group_ids.push(group_id.clone());

        // Create group members with mapped agent IDs
        for member_export in &export.members {
            let mapped_agent_id = agent_id_map.get(&member_export.agent_id).ok_or_else(|| {
                CoreError::ExportError {
                    operation: "mapping agent ID".to_string(),
                    cause: format!(
                        "Agent {} referenced in group member but not found in exports",
                        member_export.agent_id
                    ),
                }
            })?;

            self.import_group_member(member_export, &group_id, mapped_agent_id)
                .await?;
        }

        // Import shared memory blocks
        for block_cid in &export.shared_memory_cids {
            // Shared blocks get the group_id as their agent_id
            self.import_memory_block(block_cid, blocks, &group_id, options)
                .await?;
            result.memory_block_count += 1;
        }

        // Import shared block attachments
        self.import_shared_attachments(&export.shared_attachment_exports, &agent_id_map, options)
            .await?;

        Ok(result)
    }

    /// Import a thin group (configuration only, no agent data).
    async fn import_group_thin(
        &self,
        export: &GroupConfigExport,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let mut result = ImportResult::default();

        // Create the group
        let group_id = if options.preserve_ids {
            export.group.id.clone()
        } else {
            generate_id()
        };

        let group_name = options
            .rename
            .clone()
            .unwrap_or_else(|| export.group.name.clone());

        let group = self.create_group_from_record(&export.group, &group_id, &group_name)?;
        queries::create_group(&self.pool, &group).await?;
        result.group_ids.push(group_id);

        // Note: thin exports don't include agent data, so members can't be created
        // unless the agents already exist in the database. This is intentional -
        // thin exports are for configuration backup, not full restoration.

        Ok(result)
    }

    /// Create an AgentGroup from a GroupRecord.
    fn create_group_from_record(
        &self,
        record: &GroupRecord,
        id: &str,
        name: &str,
    ) -> Result<AgentGroup> {
        let now = Utc::now();
        Ok(AgentGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: record.description.clone(),
            pattern_type: record.pattern_type,
            pattern_config: Json(record.pattern_config.clone()),
            created_at: now,
            updated_at: now,
        })
    }

    /// Import a group member.
    async fn import_group_member(
        &self,
        export: &GroupMemberExport,
        group_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        let member = GroupMember {
            group_id: group_id.to_string(),
            agent_id: agent_id.to_string(),
            role: export.role.clone().map(Json),
            capabilities: Json(export.capabilities.clone()),
            joined_at: export.joined_at,
        };

        queries::add_group_member(&self.pool, &member).await?;
        Ok(())
    }

    /// Import a constellation from a CID reference.
    async fn import_constellation_from_cid(
        &self,
        data_cid: &Cid,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let data_bytes = blocks.get(data_cid).ok_or_else(|| CoreError::ExportError {
            operation: "reading constellation export".to_string(),
            cause: format!("Constellation export block {} not found", data_cid),
        })?;

        let constellation: ConstellationExport =
            decode_dag_cbor(data_bytes).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "ConstellationExport".to_string(),
                details: e.to_string(),
            })?;

        self.import_constellation(&constellation, blocks, options)
            .await
    }

    /// Import a full constellation with all agents and groups.
    async fn import_constellation(
        &self,
        export: &ConstellationExport,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let mut result = ImportResult::default();

        // Map old agent IDs to new agent IDs
        let mut agent_id_map: HashMap<String, String> = HashMap::new();

        // Import all agents from the agent_exports map
        for (old_agent_id, agent_cid) in &export.agent_exports {
            let agent_bytes = blocks
                .get(agent_cid)
                .ok_or_else(|| CoreError::ExportError {
                    operation: "reading agent export".to_string(),
                    cause: format!("Agent {} block {} not found", old_agent_id, agent_cid),
                })?;

            let agent_export: AgentExport =
                decode_dag_cbor(agent_bytes).map_err(|e| CoreError::DagCborDecodingError {
                    data_type: "AgentExport".to_string(),
                    details: e.to_string(),
                })?;

            let new_id = if options.preserve_ids {
                old_agent_id.clone()
            } else {
                generate_id()
            };

            agent_id_map.insert(old_agent_id.clone(), new_id.clone());

            // Don't use rename for constellation agents
            let agent_options = ImportOptions {
                owner_id: options.owner_id.clone(),
                rename: None,
                preserve_ids: options.preserve_ids,
                include_messages: options.include_messages,
                include_archival: options.include_archival,
            };

            let agent_result = self
                .import_agent(&agent_export, blocks, &agent_options, Some(&new_id))
                .await?;
            result.merge(agent_result);
        }

        // Import all groups
        for group_export in &export.group_exports {
            let group_result = self
                .import_group_thin_with_members(group_export, blocks, options, &agent_id_map)
                .await?;
            result.merge(group_result);
        }

        // Import additional memory blocks (orphaned/system blocks not part of agents)
        for block_cid in &export.all_memory_block_cids {
            // These blocks don't have a specific owner agent, use a placeholder
            // or the owner_id as the agent_id
            self.import_memory_block(block_cid, blocks, &options.owner_id, options)
                .await?;
            result.memory_block_count += 1;
        }

        // Import all shared block attachments
        self.import_shared_attachments(&export.shared_attachments, &agent_id_map, options)
            .await?;

        Ok(result)
    }

    /// Import a thin group from constellation with member linking.
    async fn import_group_thin_with_members(
        &self,
        export: &GroupExportThin,
        blocks: &HashMap<Cid, Vec<u8>>,
        options: &ImportOptions,
        agent_id_map: &HashMap<String, String>,
    ) -> Result<ImportResult> {
        let mut result = ImportResult::default();

        // Create the group
        let group_id = if options.preserve_ids {
            export.group.id.clone()
        } else {
            generate_id()
        };

        // For constellation groups, don't apply rename
        let group = self.create_group_from_record(&export.group, &group_id, &export.group.name)?;
        queries::create_group(&self.pool, &group).await?;
        result.group_ids.push(group_id.clone());

        // Create group members with mapped agent IDs
        for member_export in &export.members {
            let mapped_agent_id = agent_id_map.get(&member_export.agent_id).ok_or_else(|| {
                CoreError::ExportError {
                    operation: "mapping agent ID".to_string(),
                    cause: format!(
                        "Agent {} referenced in group member but not found in constellation",
                        member_export.agent_id
                    ),
                }
            })?;

            self.import_group_member(member_export, &group_id, mapped_agent_id)
                .await?;
        }

        // Import shared memory blocks
        for block_cid in &export.shared_memory_cids {
            // Shared blocks get the group_id as their agent_id
            self.import_memory_block(block_cid, blocks, &group_id, options)
                .await?;
            result.memory_block_count += 1;
        }

        // Import shared block attachments
        self.import_shared_attachments(&export.shared_attachment_exports, agent_id_map, options)
            .await?;

        Ok(result)
    }

    /// Import shared block attachments.
    ///
    /// Creates shared_block_agents records to link blocks with agents.
    /// Uses the agent_id_map to translate old agent IDs to new ones.
    async fn import_shared_attachments(
        &self,
        attachments: &[SharedBlockAttachmentExport],
        agent_id_map: &HashMap<String, String>,
        options: &ImportOptions,
    ) -> Result<()> {
        for attachment in attachments {
            // Map the agent ID
            let agent_id = if options.preserve_ids {
                attachment.agent_id.clone()
            } else {
                agent_id_map
                    .get(&attachment.agent_id)
                    .cloned()
                    .unwrap_or_else(|| attachment.agent_id.clone())
            };

            // The block_id stays the same if preserve_ids, otherwise we'd need a block_id_map
            // For now, we assume preserve_ids is needed for proper attachment restoration
            // or that the blocks were imported with the same IDs
            let block_id = attachment.block_id.clone();

            // Create the shared block attachment
            queries::create_shared_block_attachment(
                &self.pool,
                &block_id,
                &agent_id,
                attachment.permission,
            )
            .await?;
        }
        Ok(())
    }
}

/// Generate a new unique ID using UUID v4.
fn generate_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::ConstellationDb;

    async fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_importer_new() {
        let db = setup_test_db().await;
        let _importer = Importer::new(db.pool().clone());
        // Basic construction test
    }

    #[tokio::test]
    async fn test_generate_id() {
        let id1 = generate_id();
        let id2 = generate_id();
        assert_ne!(id1, id2);
        assert!(!id1.is_empty());
        // UUID simple format check (32 chars, hex)
        assert_eq!(id1.len(), 32);
        assert!(id1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_import_result_merge() {
        let mut result1 = ImportResult {
            agent_ids: vec!["agent1".to_string()],
            group_ids: vec!["group1".to_string()],
            message_count: 10,
            memory_block_count: 2,
            archival_entry_count: 5,
            archive_summary_count: 1,
        };

        let result2 = ImportResult {
            agent_ids: vec!["agent2".to_string()],
            group_ids: vec!["group2".to_string()],
            message_count: 20,
            memory_block_count: 3,
            archival_entry_count: 8,
            archive_summary_count: 2,
        };

        result1.merge(result2);

        assert_eq!(result1.agent_ids, vec!["agent1", "agent2"]);
        assert_eq!(result1.group_ids, vec!["group1", "group2"]);
        assert_eq!(result1.message_count, 30);
        assert_eq!(result1.memory_block_count, 5);
        assert_eq!(result1.archival_entry_count, 13);
        assert_eq!(result1.archive_summary_count, 3);
    }

    #[tokio::test]
    async fn test_reconstruct_empty_snapshot() {
        let db = setup_test_db().await;
        let importer = Importer::new(db.pool().clone());
        let blocks = HashMap::new();

        let result = importer.reconstruct_snapshot(&[], &blocks).unwrap();
        assert!(result.is_empty());
    }
}
