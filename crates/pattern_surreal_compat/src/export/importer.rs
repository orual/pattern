//! Agent importer implementation

use iroh_car::CarReader;
use serde_ipld_dagcbor::from_slice as decode_dag_cbor;
use std::collections::HashMap;
use tokio::io::AsyncRead;

use crate::{
    AgentId, CoreError, UserId,
    agent_entity::AgentRecord,
    export::types::{
        AgentExport, AgentRecordExport, ConstellationExport, ExportManifest, ExportType,
        GroupExport, MemoryChunk, MessageChunk,
    },
};

type Result<T> = std::result::Result<T, CoreError>;

fn reconstruct_agent_from_export(
    meta: &AgentRecordExport,
    blocks: &std::collections::HashMap<cid::Cid, Vec<u8>>,
) -> Result<AgentRecord> {
    // Gather memories
    let mut memories: Vec<(
        crate::memory::MemoryBlock,
        crate::agent_entity::AgentMemoryRelation,
    )> = Vec::new();
    for cid in &meta.memory_chunks {
        let data = blocks.get(cid).ok_or_else(|| CoreError::CarError {
            operation: "finding memory chunk".to_string(),
            cause: iroh_car::Error::Parsing(format!("block not found: {}", cid)),
        })?;
        let chunk: MemoryChunk =
            decode_dag_cbor(data).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "MemoryChunk".to_string(),
                details: format!("CID: {}, Error: {:?}", cid, e),
            })?;
        memories.extend(chunk.memories);
    }

    // Gather messages
    let mut messages: Vec<(
        crate::message::Message,
        crate::message::AgentMessageRelation,
    )> = Vec::new();
    for cid in &meta.message_chunks {
        let data = blocks.get(cid).ok_or_else(|| CoreError::CarError {
            operation: "finding message chunk".to_string(),
            cause: iroh_car::Error::Parsing(format!("block not found: {}", cid)),
        })?;
        let chunk: MessageChunk =
            decode_dag_cbor(data).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "MessageChunk".to_string(),
                details: format!("CID: {}, Error: {:?}", cid, e),
            })?;
        messages.extend(chunk.messages);
    }

    // Build a full AgentRecord
    let mut agent = AgentRecord::default();
    agent.id = meta.id.clone();
    agent.name = meta.name.clone();
    agent.agent_type = meta.agent_type.clone();
    agent.model_id = meta.model_id.clone();
    agent.model_config = meta.model_config.clone();
    agent.base_instructions = meta.base_instructions.clone();
    agent.max_messages = meta.max_messages;
    agent.max_message_age_hours = meta.max_message_age_hours;
    agent.compression_threshold = meta.compression_threshold;
    agent.memory_char_limit = meta.memory_char_limit;
    agent.enable_thinking = meta.enable_thinking;
    agent.compression_strategy = meta.compression_strategy.clone();
    agent.tool_rules = meta.tool_rules.clone();
    agent.total_messages = meta.total_messages;
    agent.total_tool_calls = meta.total_tool_calls;
    agent.context_rebuilds = meta.context_rebuilds;
    agent.compression_events = meta.compression_events;
    agent.created_at = meta.created_at;
    agent.updated_at = meta.updated_at;
    agent.last_active = meta.last_active;
    agent.owner_id = meta.owner_id.clone();
    agent.message_summary = meta.message_summary.clone();
    agent.memories = memories;
    agent.messages = messages;
    // Relations not exported
    agent.assigned_task_ids.clear();
    agent.scheduled_event_ids.clear();

    Ok(agent)
}

/// Options for importing an agent
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// New name for the imported agent (if not merging)
    pub rename_to: Option<String>,

    /// Whether to merge with existing agent (use original IDs)
    pub merge_existing: bool,

    /// Whether to preserve original IDs even when not merging
    /// If false and not merging, generates new IDs to avoid conflicts
    pub preserve_ids: bool,

    /// User ID to assign imported agents to
    pub owner_id: UserId,

    /// Whether to preserve original timestamps
    pub preserve_timestamps: bool,

    /// Whether to import messages
    pub import_messages: bool,

    /// Whether to import memories
    pub import_memories: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            rename_to: None,
            merge_existing: false,
            preserve_ids: true,
            owner_id: UserId::nil(),
            preserve_timestamps: true,
            import_messages: true,
            import_memories: true,
        }
    }
}

/// Result of an import operation
#[derive(Debug)]
pub struct ImportResult {
    /// Number of agents imported
    pub agents_imported: usize,

    /// Number of messages imported
    pub messages_imported: usize,

    /// Number of memories imported
    pub memories_imported: usize,

    /// Number of groups imported
    pub groups_imported: usize,

    /// Mapping of old agent IDs to new agent IDs
    pub agent_id_map: HashMap<AgentId, AgentId>,
}

/// Agent importer
pub struct AgentImporter<C>
where
    C: surrealdb::Connection + Clone,
{
    db: surrealdb::Surreal<C>,
}

impl<C> AgentImporter<C>
where
    C: surrealdb::Connection + Clone,
{
    /// Create a new importer
    pub fn new(db: surrealdb::Surreal<C>) -> Self {
        Self { db }
    }

    /// Detect the type of export in a CAR file
    pub async fn detect_type(
        mut input: impl AsyncRead + Unpin + Send,
    ) -> Result<(ExportType, Vec<u8>)> {
        // Read into a buffer so we can reuse it
        let mut buffer = Vec::new();
        tokio::io::copy(&mut input, &mut buffer)
            .await
            .map_err(|e| CoreError::IoError {
                operation: "reading CAR file".to_string(),
                cause: e,
            })?;

        // Create a reader from the buffer
        let mut reader = std::io::Cursor::new(&buffer);

        // Read the CAR header to get root CID
        let car_reader = CarReader::new(&mut reader)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "reading CAR header".to_string(),
                cause: e,
            })?;

        let root_cid = {
            let roots = car_reader.header().roots();
            if roots.is_empty() {
                return Err(CoreError::CarError {
                    operation: "reading CAR roots".to_string(),
                    cause: iroh_car::Error::Parsing("No root CID found".to_string()),
                });
            }
            roots[0]
        };

        // Reset reader and read blocks to find the root
        let mut reader = std::io::Cursor::new(&buffer);
        let mut car_reader =
            CarReader::new(&mut reader)
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "reading CAR header".to_string(),
                    cause: e,
                })?;

        // Find the root block
        while let Some((cid, data)) =
            car_reader
                .next_block()
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "reading CAR block".to_string(),
                    cause: e,
                })?
        {
            if cid == root_cid {
                // First try to decode as ExportManifest (new format)
                if let Ok(manifest) = decode_dag_cbor::<ExportManifest>(&data) {
                    return Ok((manifest.export_type, buffer));
                }

                // Fall back to old format detection for backwards compatibility
                if let Ok(_) = decode_dag_cbor::<AgentRecord>(&data) {
                    return Ok((ExportType::Agent, buffer));
                }
                if let Ok(_) = decode_dag_cbor::<GroupExport>(&data) {
                    return Ok((ExportType::Group, buffer));
                }
                if let Ok(_) = decode_dag_cbor::<ConstellationExport>(&data) {
                    return Ok((ExportType::Constellation, buffer));
                }

                return Err(CoreError::CarError {
                    operation: "detecting export type".to_string(),
                    cause: iroh_car::Error::Parsing("Unknown export type".to_string()),
                });
            }
        }

        Err(CoreError::CarError {
            operation: "finding root block".to_string(),
            cause: iroh_car::Error::Parsing("Root block not found".to_string()),
        })
    }

    /// Import an agent from a CAR file
    pub async fn import_agent_from_car(
        &self,
        mut input: impl AsyncRead + Unpin + Send,
        options: ImportOptions,
    ) -> Result<ImportResult> {
        // Read the CAR file
        let mut car_reader = CarReader::new(&mut input)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "reading CAR header".to_string(),
                cause: e,
            })?;

        // Get the root CID (should be the manifest)
        let root_cid = {
            let roots = car_reader.header().roots();
            if roots.is_empty() {
                return Err(CoreError::CarError {
                    operation: "reading CAR roots".to_string(),
                    cause: iroh_car::Error::Parsing("No root CID found".to_string()),
                });
            }
            roots[0]
        };

        // Read all blocks into memory
        let mut blocks = HashMap::new();

        while let Some((cid, data)) =
            car_reader
                .next_block()
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "reading CAR block".to_string(),
                    cause: e,
                })?
        {
            blocks.insert(cid, data);
        }

        // Get the root block (should be manifest)
        let root_data = blocks.get(&root_cid).ok_or_else(|| CoreError::CarError {
            operation: "finding root block".to_string(),
            cause: iroh_car::Error::Parsing(format!("Root block not found for CID: {}", root_cid)),
        })?;

        // Try to decode as manifest first (new format)
        let agent_export_cid = if let Ok(manifest) = decode_dag_cbor::<ExportManifest>(root_data) {
            // New format - get the data CID from manifest
            manifest.data_cid
        } else {
            // Old format - root is the agent directly
            root_cid
        };

        // Get the agent export block
        let agent_export_data =
            blocks
                .get(&agent_export_cid)
                .ok_or_else(|| CoreError::CarError {
                    operation: "finding agent export block".to_string(),
                    cause: iroh_car::Error::Parsing(format!(
                        "Agent export block not found for CID: {}",
                        agent_export_cid
                    )),
                })?;

        // Decode AgentExport and then the slim AgentRecordExport
        let mut agent: AgentRecord =
            if let Ok(agent_export) = decode_dag_cbor::<AgentExport>(agent_export_data) {
                let meta_cid = agent_export.agent_cid;
                let meta_block = blocks.get(&meta_cid).ok_or_else(|| CoreError::CarError {
                    operation: "finding agent metadata block".to_string(),
                    cause: iroh_car::Error::Parsing(format!(
                        "Agent metadata block not found for CID: {}",
                        meta_cid
                    )),
                })?;
                let meta: AgentRecordExport =
                    decode_dag_cbor(meta_block).map_err(|e| CoreError::DagCborDecodingError {
                        data_type: "AgentRecordExport".to_string(),
                        details: format!("Meta CID: {}, Error: {:?}", meta_cid, e),
                    })?;
                reconstruct_agent_from_export(&meta, &blocks)?
            } else {
                // Legacy fallback: decode directly as AgentRecord if present
                decode_dag_cbor(agent_export_data).map_err(|e| CoreError::DagCborDecodingError {
                    data_type: "AgentRecord".to_string(),
                    details: format!("{:?}", e),
                })?
            };

        // Store the original ID for mapping
        let original_id = agent.id.clone();

        // Handle agent import based on options
        if options.merge_existing || options.preserve_ids {
            // Keep original ID
            // If merge_existing is true, we'll update the existing agent
            // If preserve_ids is true, we'll create a new agent with the same ID
        } else {
            // Generate new ID for the agent
            agent.id = AgentId::generate();
        }

        // Update name if requested
        if let Some(new_name) = options.rename_to {
            agent.name = new_name;
        }

        // Update owner
        agent.owner_id = options.owner_id.clone();

        // Update timestamps if not preserving
        if !options.preserve_timestamps {
            let now = chrono::Utc::now();
            agent.created_at = now;
            agent.updated_at = now;
            agent.last_active = now;
        }

        // Filter memories if requested
        if !options.import_memories {
            agent.memories.clear();
        }

        // Filter messages if requested
        if !options.import_messages {
            agent.messages.clear();
        }

        // Store counts before storing
        let memory_count = agent.memories.len();
        let message_count = agent.messages.len();

        // Store the agent with all its relations individually to avoid payload limits
        let stored_agent = agent
            .store_with_relations_individually(&self.db)
            .await
            .map_err(|e| CoreError::from(e))?;

        let mut result = ImportResult {
            agents_imported: 1,
            messages_imported: message_count,
            memories_imported: memory_count,
            groups_imported: 0,
            agent_id_map: HashMap::new(),
        };

        result
            .agent_id_map
            .insert(original_id, stored_agent.id.clone());

        Ok(result)
    }

    /// Import a group from a CAR file
    pub async fn import_group_from_car(
        &self,
        mut input: impl AsyncRead + Unpin + Send,
        options: ImportOptions,
    ) -> Result<ImportResult> {
        // Read the CAR file
        let mut car_reader = CarReader::new(&mut input)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "reading CAR header".to_string(),
                cause: e,
            })?;

        let root_cid = {
            let roots = car_reader.header().roots();
            if roots.is_empty() {
                return Err(CoreError::CarError {
                    operation: "reading CAR roots".to_string(),
                    cause: iroh_car::Error::Parsing("No root CID found".to_string()),
                });
            }
            roots[0]
        };

        // Read all blocks
        let mut blocks = HashMap::new();

        while let Some((cid, data)) =
            car_reader
                .next_block()
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "reading CAR block".to_string(),
                    cause: e,
                })?
        {
            blocks.insert(cid, data);
        }

        // Get the root block
        let root_data = blocks.get(&root_cid).ok_or_else(|| CoreError::CarError {
            operation: "finding root block".to_string(),
            cause: iroh_car::Error::Parsing(format!("Root block not found for CID: {}", root_cid)),
        })?;

        // Try to decode as manifest first (new format)
        let group_export_cid = if let Ok(manifest) = decode_dag_cbor::<ExportManifest>(root_data) {
            // New format - get the data CID from manifest
            manifest.data_cid
        } else {
            // Old format - root is the group export directly
            root_cid
        };

        // Get the group export block
        let group_export_data =
            blocks
                .get(&group_export_cid)
                .ok_or_else(|| CoreError::CarError {
                    operation: "finding group export block".to_string(),
                    cause: iroh_car::Error::Parsing(format!(
                        "Group export block not found for CID: {}",
                        group_export_cid
                    )),
                })?;

        // Decode the group export
        let group_export: GroupExport =
            decode_dag_cbor(group_export_data).map_err(|e| CoreError::DagCborDecodingError {
                data_type: "GroupExport".to_string(),
                details: e.to_string(),
            })?;

        let mut result = ImportResult {
            agents_imported: 0,
            messages_imported: 0,
            memories_imported: 0,
            groups_imported: 0,
            agent_id_map: HashMap::new(),
        };

        // First import all member agents and preserve their membership data
        let mut imported_memberships = Vec::new();

        for (_old_agent_id, agent_export_cid) in &group_export.member_agent_cids {
            if let Some(agent_export_data) = blocks.get(agent_export_cid) {
                // New format: AgentExport -> AgentRecordExport -> reconstruct
                let mut agent: AgentRecord =
                    if let Ok(export) = decode_dag_cbor::<AgentExport>(agent_export_data) {
                        let meta_block =
                            blocks
                                .get(&export.agent_cid)
                                .ok_or_else(|| CoreError::CarError {
                                    operation: "finding agent metadata block".to_string(),
                                    cause: iroh_car::Error::Parsing(format!(
                                        "Agent metadata block not found for CID: {}",
                                        export.agent_cid
                                    )),
                                })?;
                        let meta: AgentRecordExport = decode_dag_cbor(meta_block).map_err(|e| {
                            CoreError::DagCborDecodingError {
                                data_type: "AgentRecordExport".to_string(),
                                details: e.to_string(),
                            }
                        })?;
                        reconstruct_agent_from_export(&meta, &blocks)?
                    } else {
                        // Legacy fallback
                        decode_dag_cbor(agent_export_data).map_err(|e| {
                            CoreError::DagCborDecodingError {
                                data_type: "AgentRecord".to_string(),
                                details: e.to_string(),
                            }
                        })?
                    };

                // Store the original ID
                let original_id = agent.id.clone();

                // Determine new ID based on options
                if !(options.merge_existing || options.preserve_ids) {
                    agent.id = AgentId::generate();
                }

                agent.owner_id = options.owner_id.clone();

                // Update timestamps if not preserving
                if !options.preserve_timestamps {
                    let now = chrono::Utc::now();
                    agent.created_at = now;
                    agent.updated_at = now;
                    agent.last_active = now;
                }

                // Filter memories/messages based on options
                if !options.import_memories {
                    agent.memories.clear();
                }
                if !options.import_messages {
                    agent.messages.clear();
                }

                // Store the agent with relations individually to avoid payload limits
                let stored_agent = agent
                    .store_with_relations_individually(&self.db)
                    .await
                    .map_err(|e| CoreError::from(e))?;

                // Find and preserve the original membership data for this agent
                let original_membership = group_export
                    .member_memberships
                    .iter()
                    .find(|(a_id, _)| a_id == &original_id)
                    .map(|(_, membership)| membership.clone());

                result
                    .agent_id_map
                    .insert(original_id, stored_agent.id.clone());
                result.agents_imported += 1;
                result.memories_imported += agent.memories.len();
                result.messages_imported += agent.messages.len();

                if let Some(membership) = original_membership {
                    imported_memberships.push((stored_agent.id.clone(), membership));
                }
            }
        }

        // Import the group itself with updated member references
        let mut group = group_export.group;

        // Store original ID for potential future use
        let _original_group_id = group.id.clone();

        // Update name if requested
        if let Some(new_name) = options.rename_to {
            group.name = new_name;
        }

        // Handle group ID based on options
        if !(options.merge_existing || options.preserve_ids) {
            group.id = crate::id::GroupId::generate();
        }

        // Update timestamps if not preserving
        if !options.preserve_timestamps {
            let now = chrono::Utc::now();
            group.created_at = now;
            group.updated_at = now;
        }

        // Clear members - we'll re-add them with new IDs
        group.members.clear();

        // Store the base group first
        let created_group = crate::db::ops::create_group(&self.db, &group)
            .await
            .map_err(|e| CoreError::from(e))?;

        // Re-add members with their preserved membership data
        for (new_agent_id, mut original_membership) in imported_memberships {
            // Update the membership with new IDs
            original_membership.id = crate::id::RelationId::nil();
            original_membership.in_id = new_agent_id;
            original_membership.out_id = created_group.id.clone();

            // Update timestamp if not preserving
            if !options.preserve_timestamps {
                original_membership.joined_at = chrono::Utc::now();
            }

            crate::db::ops::add_agent_to_group(&self.db, &original_membership)
                .await
                .map_err(|e| CoreError::from(e))?;
        }

        result.groups_imported = 1;

        Ok(result)
    }

    /// Import a constellation from a CAR file
    pub async fn import_constellation_from_car(
        &self,
        mut input: impl AsyncRead + Unpin + Send,
        options: ImportOptions,
    ) -> Result<ImportResult> {
        // Read the CAR file
        let mut car_reader = CarReader::new(&mut input)
            .await
            .map_err(|e| CoreError::CarError {
                operation: "reading CAR header".to_string(),
                cause: e,
            })?;

        let root_cid = {
            let roots = car_reader.header().roots();
            if roots.is_empty() {
                return Err(CoreError::CarError {
                    operation: "reading CAR roots".to_string(),
                    cause: iroh_car::Error::Parsing("No root CID found".to_string()),
                });
            }
            roots[0]
        };

        // Read all blocks
        let mut blocks = HashMap::new();

        while let Some((cid, data)) =
            car_reader
                .next_block()
                .await
                .map_err(|e| CoreError::CarError {
                    operation: "reading CAR block".to_string(),
                    cause: e,
                })?
        {
            blocks.insert(cid, data);
        }

        // Get the root block
        let root_data = blocks.get(&root_cid).ok_or_else(|| CoreError::CarError {
            operation: "finding root block".to_string(),
            cause: iroh_car::Error::Parsing(format!("Root block not found for CID: {}", root_cid)),
        })?;

        // Try to decode as manifest first (new format)
        let constellation_export_cid =
            if let Ok(manifest) = decode_dag_cbor::<ExportManifest>(root_data) {
                // New format - get the data CID from manifest
                manifest.data_cid
            } else {
                // Old format - root is the constellation export directly
                root_cid
            };

        // Get the constellation export block
        let constellation_export_data =
            blocks
                .get(&constellation_export_cid)
                .ok_or_else(|| CoreError::CarError {
                    operation: "finding constellation export block".to_string(),
                    cause: iroh_car::Error::Parsing(format!(
                        "Constellation export block not found for CID: {}",
                        constellation_export_cid
                    )),
                })?;

        // Decode the constellation export
        let constellation_export: ConstellationExport = decode_dag_cbor(constellation_export_data)
            .map_err(|e| CoreError::DagCborDecodingError {
                data_type: "ConstellationExport".to_string(),
                details: e.to_string(),
            })?;

        let mut result = ImportResult {
            agents_imported: 0,
            messages_imported: 0,
            memories_imported: 0,
            groups_imported: 0,
            agent_id_map: HashMap::new(),
        };

        // Import all agents first
        for (_old_agent_id, agent_export_cid) in &constellation_export.agent_export_cids {
            if let Some(agent_export_data) = blocks.get(agent_export_cid) {
                let mut agent: AgentRecord =
                    if let Ok(export) = decode_dag_cbor::<AgentExport>(agent_export_data) {
                        let meta_block =
                            blocks
                                .get(&export.agent_cid)
                                .ok_or_else(|| CoreError::CarError {
                                    operation: "finding agent metadata block".to_string(),
                                    cause: iroh_car::Error::Parsing(format!(
                                        "Agent metadata block not found for CID: {}",
                                        export.agent_cid
                                    )),
                                })?;
                        let meta: AgentRecordExport = decode_dag_cbor(meta_block).map_err(|e| {
                            CoreError::DagCborDecodingError {
                                data_type: "AgentRecordExport".to_string(),
                                details: e.to_string(),
                            }
                        })?;
                        reconstruct_agent_from_export(&meta, &blocks)?
                    } else {
                        decode_dag_cbor(agent_export_data).map_err(|e| {
                            CoreError::DagCborDecodingError {
                                data_type: "AgentRecord".to_string(),
                                details: e.to_string(),
                            }
                        })?
                    };

                // Store original ID
                let original_id = agent.id.clone();

                // Handle ID based on options
                if !(options.merge_existing || options.preserve_ids) {
                    agent.id = AgentId::generate();
                }

                agent.owner_id = options.owner_id.clone();

                // Update timestamps if not preserving
                if !options.preserve_timestamps {
                    let now = chrono::Utc::now();
                    agent.created_at = now;
                    agent.updated_at = now;
                    agent.last_active = now;
                }

                // Filter memories/messages based on options
                if !options.import_memories {
                    agent.memories.clear();
                }
                if !options.import_messages {
                    agent.messages.clear();
                }

                // Store the agent with relations individually to avoid payload limits
                let stored_agent = agent
                    .store_with_relations_individually(&self.db)
                    .await
                    .map_err(|e| CoreError::from(e))?;

                result
                    .agent_id_map
                    .insert(original_id, stored_agent.id.clone());
                result.agents_imported += 1;
                result.memories_imported += agent.memories.len();
                result.messages_imported += agent.messages.len();
            }
        }

        // Import all groups with updated agent references
        let mut group_id_map = HashMap::new();

        for group_export in &constellation_export.groups {
            let mut group = group_export.group.clone();
            let original_group_id = group.id.clone();

            // Handle group ID based on options
            if !(options.merge_existing || options.preserve_ids) {
                group.id = crate::id::GroupId::generate();
            }

            // Update timestamps if not preserving
            if !options.preserve_timestamps {
                let now = chrono::Utc::now();
                group.created_at = now;
                group.updated_at = now;
            }

            // Clear members - we'll re-add them with new IDs and preserved roles
            group.members.clear();

            // Create the group
            let created_group = crate::db::ops::create_group(&self.db, &group)
                .await
                .map_err(|e| CoreError::from(e))?;

            group_id_map.insert(original_group_id, created_group.id.clone());

            // Re-add members with preserved membership data
            for (original_agent_id, _) in &group_export.member_agent_cids {
                if let Some(new_agent_id) = result.agent_id_map.get(original_agent_id) {
                    // Find the original membership data
                    if let Some((_, original_membership)) = group_export
                        .group
                        .members
                        .iter()
                        .find(|(a, _)| &a.id == original_agent_id)
                    {
                        let mut membership = original_membership.clone();
                        membership.id = crate::id::RelationId::generate();
                        membership.in_id = new_agent_id.clone();
                        membership.out_id = created_group.id.clone();

                        if !options.preserve_timestamps {
                            membership.joined_at = chrono::Utc::now();
                        }

                        crate::db::ops::add_agent_to_group(&self.db, &membership)
                            .await
                            .map_err(|e| CoreError::from(e))?;
                    }
                }
            }

            result.groups_imported += 1;
        }

        // Import the constellation itself
        let mut constellation = constellation_export.constellation;

        // Handle constellation ID based on options
        if !(options.merge_existing || options.preserve_ids) {
            constellation.id = crate::id::ConstellationId::generate();
        }

        constellation.owner_id = options.owner_id.clone();

        // Update timestamps if not preserving
        if !options.preserve_timestamps {
            let now = chrono::Utc::now();
            constellation.created_at = now;
            constellation.updated_at = now;
        }

        // Update group IDs to new ones
        constellation.groups = constellation
            .groups
            .into_iter()
            .filter_map(|old_id| group_id_map.get(&old_id).cloned())
            .collect();

        // Clear agents - we'll re-add them
        constellation.agents.clear();

        // Create the constellation
        let created_constellation =
            crate::db::ops::create_entity::<crate::groups::Constellation, _>(
                &self.db,
                &constellation,
            )
            .await
            .map_err(|e| CoreError::from(e))?;

        // Add agents to constellation using edge entities
        for (_, new_agent_id) in &result.agent_id_map {
            let membership = crate::groups::ConstellationMembership {
                id: crate::id::RelationId::nil(),
                in_id: created_constellation.id.clone(),
                out_id: new_agent_id.clone(),
                joined_at: chrono::Utc::now(),
                is_primary: false, // Could be preserved from original if needed
            };

            crate::db::ops::create_relation_typed(&self.db, &membership)
                .await
                .map_err(|e| CoreError::from(e))?;
        }

        // Add groups to constellation
        for (_, new_group_id) in &group_id_map {
            crate::db::ops::add_group_to_constellation(
                &self.db,
                &created_constellation.id,
                new_group_id,
            )
            .await
            .map_err(|e| CoreError::from(e))?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::client;
    use crate::export::{AgentExporter, ExportOptions};

    #[tokio::test]
    async fn import_roundtrip_agent() {
        // Build a simple agent with enough data to create multiple chunks
        use crate::agent_entity::AgentRecord;
        use crate::id::RelationId;
        use crate::memory::{MemoryBlock, MemoryPermission, MemoryType};
        use crate::message::{AgentMessageRelation, Message, MessageRelationType};
        use chrono::Utc;

        let db = client::create_test_db().await.unwrap();
        let exporter = AgentExporter::new(db.clone());

        // Fabricate an agent in-memory
        let mut agent = AgentRecord {
            name: "RoundTrip".to_string(),
            owner_id: crate::UserId::generate(),
            ..Default::default()
        };
        let mut msgs = Vec::new();
        for i in 0..1200usize {
            tokio::time::sleep(std::time::Duration::from_micros(500)).await;
            let m = Message::user(format!("msg{}", i));
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
        let mut mems = Vec::new();
        for i in 0..120usize {
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

        // Export to blocks (no CAR), then reconstruct via helper
        let (agent_export, blocks, _stats) = exporter
            .export_agent_to_blocks(&agent, &ExportOptions::default())
            .await
            .unwrap();

        // Build block map and decode meta
        let map: std::collections::HashMap<_, _> = blocks.iter().cloned().collect();
        let meta_bytes = map
            .get(&agent_export.agent_cid)
            .expect("agent meta present");
        let meta: AgentRecordExport = serde_ipld_dagcbor::from_slice(meta_bytes).unwrap();

        // Reconstruct and validate sizes
        let reconstructed = super::reconstruct_agent_from_export(&meta, &map).unwrap();
        assert!(reconstructed.messages.len() >= 1200);
        assert!(reconstructed.memories.len() >= 120);
    }
}
