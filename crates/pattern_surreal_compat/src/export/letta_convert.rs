//! Letta Agent File (.af) to Pattern v3 CAR converter.
//!
//! Converts Letta's JSON-based agent file format to Pattern's CAR export format.
//! This is a one-way conversion - Pattern uses Loro CRDTs for memory which cannot
//! be losslessly converted back to Letta's plain text format.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use chrono::Utc;
use cid::Cid;
use thiserror::Error;
use tokio::fs::File;
use tracing::info;

use pattern_db::models::{
    AgentStatus, BatchType, MemoryBlockType, MemoryPermission, MessageRole, PatternType,
};

use super::letta_types::{
    AgentFileSchema, AgentSchema, BlockSchema, CreateBlockSchema, GroupSchema, MessageSchema,
    ToolMapping,
};
use super::{
    AgentExport, AgentRecord, EXPORT_VERSION, ExportManifest, ExportStats, ExportType, GroupExport,
    GroupMemberExport, GroupRecord, MemoryBlockExport, MessageChunk, MessageExport,
    SharedBlockAttachmentExport, SnapshotChunk, TARGET_CHUNK_BYTES, encode_block,
};

/// Errors that can occur during Letta conversion.
#[derive(Debug, Error)]
pub enum LettaConversionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("CAR encoding error: {0}")]
    Encoding(String),

    #[error("No agents found in agent file")]
    NoAgents,

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Block not found: {0}")]
    BlockNotFound(String),
}

/// Statistics about a Letta conversion.
#[derive(Debug, Clone, Default)]
pub struct LettaConversionStats {
    pub agents_converted: u64,
    pub groups_converted: u64,
    pub messages_converted: u64,
    pub memory_blocks_converted: u64,
    pub tools_mapped: u64,
    pub tools_dropped: u64,
}

/// Options for Letta conversion.
#[derive(Debug, Clone)]
pub struct LettaConversionOptions {
    /// Owner ID to assign to imported entities
    pub owner_id: String,

    /// Whether to include message history
    pub include_messages: bool,

    /// Rename the primary agent (if single agent export)
    pub rename: Option<String>,
}

impl Default for LettaConversionOptions {
    fn default() -> Self {
        Self {
            owner_id: "imported".to_string(),
            include_messages: true,
            rename: None,
        }
    }
}

/// Convert a Letta .af file to Pattern v3 CAR format.
pub async fn convert_letta_to_car(
    input_path: &Path,
    output_path: &Path,
    options: &LettaConversionOptions,
) -> Result<LettaConversionStats, LettaConversionError> {
    info!(
        "Converting Letta agent file {} to {}",
        input_path.display(),
        output_path.display()
    );

    // Read and parse the JSON file
    let mut file = std::fs::File::open(input_path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let agent_file: AgentFileSchema = serde_json::from_str(&contents)?;

    if agent_file.agents.is_empty() {
        return Err(LettaConversionError::NoAgents);
    }

    // Convert
    let (manifest, blocks, stats) = convert_agent_file(&agent_file, options)?;

    // Write CAR file
    write_car_file(output_path, manifest, blocks).await?;

    info!(
        "Conversion complete: {} agents, {} messages, {} memory blocks",
        stats.agents_converted, stats.messages_converted, stats.memory_blocks_converted
    );

    Ok(stats)
}

/// Convert an AgentFileSchema to CAR blocks.
fn convert_agent_file(
    agent_file: &AgentFileSchema,
    options: &LettaConversionOptions,
) -> Result<(ExportManifest, Vec<(Cid, Vec<u8>)>, LettaConversionStats), LettaConversionError> {
    let mut all_blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut stats = LettaConversionStats::default();

    // Build block lookup from top-level blocks
    let block_lookup: HashMap<String, &BlockSchema> = agent_file
        .blocks
        .iter()
        .map(|b| (b.id.clone(), b))
        .collect();

    // Determine export type based on content
    let (data_cid, export_type) = if agent_file.groups.is_empty() {
        if agent_file.agents.len() == 1 {
            // Single agent export
            let agent = &agent_file.agents[0];
            let result = convert_agent(agent, &block_lookup, &agent_file.tools, options)?;
            all_blocks.extend(result.blocks);
            stats.agents_converted = 1;
            stats.messages_converted = result.message_count;
            stats.memory_blocks_converted = result.memory_count;
            stats.tools_mapped = result.tools_mapped;
            stats.tools_dropped = result.tools_dropped;
            (result.export_cid, ExportType::Agent)
        } else {
            // Multiple agents without groups - create a synthetic group
            let result = convert_agents_to_group(
                &agent_file.agents,
                &block_lookup,
                &agent_file.tools,
                options,
            )?;
            all_blocks.extend(result.blocks);
            stats.agents_converted = agent_file.agents.len() as u64;
            stats.messages_converted = result.message_count;
            stats.memory_blocks_converted = result.memory_count;
            stats.groups_converted = 1;
            (result.export_cid, ExportType::Group)
        }
    } else {
        // Has groups - export first group (could extend to full constellation later)
        let group = &agent_file.groups[0];
        let result = convert_group(
            group,
            &agent_file.agents,
            &block_lookup,
            &agent_file.tools,
            options,
        )?;
        all_blocks.extend(result.blocks);
        stats.agents_converted = result.agent_count;
        stats.messages_converted = result.message_count;
        stats.memory_blocks_converted = result.memory_count;
        stats.groups_converted = 1;
        (result.export_cid, ExportType::Group)
    };

    // Create manifest
    let manifest = ExportManifest {
        version: EXPORT_VERSION,
        exported_at: Utc::now(),
        export_type,
        stats: ExportStats {
            agent_count: stats.agents_converted,
            group_count: stats.groups_converted,
            message_count: stats.messages_converted,
            memory_block_count: stats.memory_blocks_converted,
            archival_entry_count: 0,
            archive_summary_count: 0,
            chunk_count: 0,
            total_blocks: all_blocks.len() as u64 + 1,
            total_bytes: all_blocks.iter().map(|(_, d)| d.len() as u64).sum(),
        },
        data_cid,
    };

    Ok((manifest, all_blocks, stats))
}

/// Result of converting an agent.
struct AgentConversionResult {
    export_cid: Cid,
    blocks: Vec<(Cid, Vec<u8>)>,
    message_count: u64,
    memory_count: u64,
    tools_mapped: u64,
    tools_dropped: u64,
}

/// Convert a single Letta agent to Pattern format.
fn convert_agent(
    agent: &AgentSchema,
    block_lookup: &HashMap<String, &BlockSchema>,
    all_tools: &[super::letta_types::ToolSchema],
    options: &LettaConversionOptions,
) -> Result<AgentConversionResult, LettaConversionError> {
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut tools_mapped = 0u64;
    let mut tools_dropped = 0u64;

    // Build enabled tools list
    let enabled_tools = ToolMapping::build_enabled_tools(agent, all_tools);

    // Count tool mapping stats
    for tool_id in &agent.tool_ids {
        if let Some(tool) = all_tools.iter().find(|t| &t.id == tool_id) {
            if let Some(ref name) = tool.name {
                if ToolMapping::map_tool(name).is_some() {
                    tools_mapped += 1;
                } else {
                    tools_dropped += 1;
                }
            }
        }
    }

    // Parse model provider/name from "provider/model-name" format
    let (model_provider, model_name) = parse_model_string(agent);

    // Create agent record
    let agent_name = options
        .rename
        .clone()
        .or_else(|| agent.name.clone())
        .unwrap_or_else(|| format!("letta-{}", &agent.id[..8.min(agent.id.len())]));

    let agent_record = AgentRecord {
        id: agent.id.clone(),
        name: agent_name,
        description: agent.description.clone(),
        model_provider,
        model_name,
        system_prompt: agent.system.clone().unwrap_or_default(),
        config: build_agent_config(agent),
        enabled_tools,
        tool_rules: if agent.tool_rules.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&agent.tool_rules).unwrap_or_default())
        },
        status: AgentStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    // Convert memory blocks
    let mut memory_block_cids: Vec<Cid> = Vec::new();

    // Inline memory_blocks
    for block in &agent.memory_blocks {
        let (cid, block_data) = convert_inline_block(block, &agent.id)?;
        blocks.extend(block_data);
        memory_block_cids.push(cid);
    }

    // Referenced block_ids
    for block_id in &agent.block_ids {
        if let Some(block) = block_lookup.get(block_id) {
            let (cid, block_data) = convert_block(block, &agent.id)?;
            blocks.extend(block_data);
            memory_block_cids.push(cid);
        }
    }

    let memory_count = memory_block_cids.len() as u64;

    // Convert messages
    let (message_chunk_cids, message_blocks, message_count) = if options.include_messages {
        convert_messages(&agent.messages, &agent.id)?
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    blocks.extend(message_blocks);

    // Create agent export
    let agent_export = AgentExport {
        agent: agent_record,
        message_chunk_cids,
        memory_block_cids,
        archival_entry_cids: Vec::new(),
        archive_summary_cids: Vec::new(),
    };

    let (export_cid, export_data) = encode_block(&agent_export, "AgentExport")
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
    blocks.push((export_cid, export_data));

    Ok(AgentConversionResult {
        export_cid,
        blocks,
        message_count,
        memory_count,
        tools_mapped,
        tools_dropped,
    })
}

/// Result of converting a group.
struct GroupConversionResult {
    export_cid: Cid,
    blocks: Vec<(Cid, Vec<u8>)>,
    agent_count: u64,
    message_count: u64,
    memory_count: u64,
}

/// Convert a Letta group to Pattern format.
fn convert_group(
    group: &GroupSchema,
    all_agents: &[AgentSchema],
    block_lookup: &HashMap<String, &BlockSchema>,
    all_tools: &[super::letta_types::ToolSchema],
    options: &LettaConversionOptions,
) -> Result<GroupConversionResult, LettaConversionError> {
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut total_messages = 0u64;
    let mut total_memory = 0u64;

    // Convert member agents
    let mut agent_exports: Vec<AgentExport> = Vec::new();
    let mut members: Vec<GroupMemberExport> = Vec::new();

    for agent_id in &group.agent_ids {
        let agent = all_agents
            .iter()
            .find(|a| &a.id == agent_id)
            .ok_or_else(|| LettaConversionError::AgentNotFound(agent_id.clone()))?;

        let result = convert_agent(agent, block_lookup, all_tools, options)?;
        total_messages += result.message_count;
        total_memory += result.memory_count;

        // Extract the AgentExport from blocks
        let agent_export_data = result
            .blocks
            .iter()
            .find(|(cid, _)| cid == &result.export_cid)
            .map(|(_, data)| data.clone())
            .ok_or_else(|| LettaConversionError::Encoding("Missing agent export".to_string()))?;

        let agent_export: AgentExport = serde_ipld_dagcbor::from_slice(&agent_export_data)
            .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

        // Add all blocks except the agent export itself (we'll inline it)
        for (cid, data) in result.blocks {
            if cid != result.export_cid {
                blocks.push((cid, data));
            }
        }

        members.push(GroupMemberExport {
            group_id: group.id.clone(),
            agent_id: agent_id.clone(),
            role: None,
            capabilities: Vec::new(),
            joined_at: Utc::now(),
        });

        agent_exports.push(agent_export);
    }

    // Convert shared blocks
    let mut shared_memory_cids: Vec<Cid> = Vec::new();
    let mut shared_attachments: Vec<SharedBlockAttachmentExport> = Vec::new();

    for block_id in &group.shared_block_ids {
        if let Some(block) = block_lookup.get(block_id) {
            // Use first agent as "owner"
            let owner_id = group
                .agent_ids
                .first()
                .map(|s| s.as_str())
                .unwrap_or("shared");
            let (cid, block_data) = convert_block(block, owner_id)?;
            blocks.extend(block_data);
            shared_memory_cids.push(cid);

            // Create attachments for other agents
            for agent_id in group.agent_ids.iter().skip(1) {
                shared_attachments.push(SharedBlockAttachmentExport {
                    block_id: block_id.clone(),
                    agent_id: agent_id.clone(),
                    permission: MemoryPermission::ReadWrite,
                    attached_at: Utc::now(),
                });
            }
        }
    }

    // Create group record
    let group_record = GroupRecord {
        id: group.id.clone(),
        name: group
            .description
            .clone()
            .unwrap_or_else(|| format!("letta-group-{}", &group.id[..8.min(group.id.len())])),
        description: group.description.clone(),
        pattern_type: PatternType::Dynamic, // Letta groups map best to dynamic routing
        pattern_config: group.manager_config.clone().unwrap_or_default(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    // Create group export
    let group_export = GroupExport {
        group: group_record,
        members,
        agent_exports,
        shared_memory_cids,
        shared_attachment_exports: shared_attachments,
    };

    let (export_cid, export_data) = encode_block(&group_export, "GroupExport")
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
    blocks.push((export_cid, export_data));

    Ok(GroupConversionResult {
        export_cid,
        blocks,
        agent_count: group.agent_ids.len() as u64,
        message_count: total_messages,
        memory_count: total_memory,
    })
}

/// Convert multiple standalone agents to a synthetic group.
fn convert_agents_to_group(
    agents: &[AgentSchema],
    block_lookup: &HashMap<String, &BlockSchema>,
    all_tools: &[super::letta_types::ToolSchema],
    options: &LettaConversionOptions,
) -> Result<GroupConversionResult, LettaConversionError> {
    // Create a synthetic group containing all agents
    let synthetic_group = GroupSchema {
        id: format!("letta-import-{}", Utc::now().timestamp()),
        agent_ids: agents.iter().map(|a| a.id.clone()).collect(),
        description: Some("Imported from Letta agent file".to_string()),
        manager_config: None,
        project_id: None,
        shared_block_ids: Vec::new(),
    };

    convert_group(&synthetic_group, agents, block_lookup, all_tools, options)
}

/// Convert a top-level BlockSchema to MemoryBlockExport.
fn convert_block(
    block: &BlockSchema,
    agent_id: &str,
) -> Result<(Cid, Vec<(Cid, Vec<u8>)>), LettaConversionError> {
    let value = block.value.as_deref().unwrap_or("");
    let label = block.label.as_deref().unwrap_or("unnamed");

    let loro_snapshot = text_to_loro_snapshot(value);
    let total_bytes = loro_snapshot.len() as u64;

    let (snapshot_cids, snapshot_blocks) = chunk_snapshot(loro_snapshot)?;

    let block_type = label_to_block_type(label);
    let permission = if block.read_only.unwrap_or(false) {
        MemoryPermission::ReadOnly
    } else {
        MemoryPermission::ReadWrite
    };

    let export = MemoryBlockExport {
        id: block.id.clone(),
        agent_id: agent_id.to_string(),
        label: label.to_string(),
        description: block.description.clone().unwrap_or_default(),
        block_type,
        char_limit: block.limit.unwrap_or(5000),
        permission,
        pinned: false,
        content_preview: Some(value.to_string()),
        metadata: block.metadata.clone(),
        is_active: true,
        frontier: None,
        last_seq: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        snapshot_chunk_cids: snapshot_cids.clone(),
        total_snapshot_bytes: total_bytes,
    };

    let (cid, data) = encode_block(&export, "MemoryBlockExport")
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

    let mut all_blocks = snapshot_blocks;
    all_blocks.push((cid, data));

    Ok((cid, all_blocks))
}

/// Convert an inline CreateBlockSchema to MemoryBlockExport.
fn convert_inline_block(
    block: &CreateBlockSchema,
    agent_id: &str,
) -> Result<(Cid, Vec<(Cid, Vec<u8>)>), LettaConversionError> {
    let value = block.value.as_deref().unwrap_or("");
    let label = block.label.as_deref().unwrap_or("unnamed");
    let block_id = format!("block-{}-{}", agent_id, label);

    let loro_snapshot = text_to_loro_snapshot(value);
    let total_bytes = loro_snapshot.len() as u64;

    let (snapshot_cids, snapshot_blocks) = chunk_snapshot(loro_snapshot)?;

    let block_type = label_to_block_type(label);
    let permission = if block.read_only.unwrap_or(false) {
        MemoryPermission::ReadOnly
    } else {
        MemoryPermission::ReadWrite
    };

    let export = MemoryBlockExport {
        id: block_id,
        agent_id: agent_id.to_string(),
        label: label.to_string(),
        description: block.description.clone().unwrap_or_default(),
        block_type,
        char_limit: block.limit.unwrap_or(5000),
        permission,
        pinned: false,
        content_preview: Some(value.to_string()),
        metadata: block.metadata.clone(),
        is_active: true,
        frontier: None,
        last_seq: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        snapshot_chunk_cids: snapshot_cids.clone(),
        total_snapshot_bytes: total_bytes,
    };

    let (cid, data) = encode_block(&export, "MemoryBlockExport")
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

    let mut all_blocks = snapshot_blocks;
    all_blocks.push((cid, data));

    Ok((cid, all_blocks))
}

/// Convert Letta messages to Pattern message chunks.
fn convert_messages(
    messages: &[MessageSchema],
    agent_id: &str,
) -> Result<(Vec<Cid>, Vec<(Cid, Vec<u8>)>, u64), LettaConversionError> {
    if messages.is_empty() {
        return Ok((Vec::new(), Vec::new(), 0));
    }

    let mut converted: Vec<MessageExport> = Vec::new();
    let now = Utc::now();

    for (idx, msg) in messages.iter().enumerate() {
        // Generate snowflake-style position from index
        let position = format!("{:020}", idx);
        let batch_id = format!("letta-import-{}", now.timestamp());

        let role = match msg
            .role
            .as_deref()
            .unwrap_or("user")
            .to_lowercase()
            .as_str()
        {
            "system" => MessageRole::System,
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            _ => MessageRole::User,
        };

        // Build content JSON
        let content_json = if let Some(ref content) = msg.content {
            content.clone()
        } else if let Some(ref text) = msg.text {
            serde_json::json!([{"type": "text", "text": text}])
        } else {
            serde_json::json!([])
        };

        // Extract text preview
        let content_preview = msg.text.clone().or_else(|| {
            msg.content.as_ref().and_then(|c| {
                if let Some(text) = c.as_str() {
                    Some(text.to_string())
                } else if let Some(arr) = c.as_array() {
                    arr.iter()
                        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                        .next()
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        });

        converted.push(MessageExport {
            id: msg.id.clone(),
            agent_id: agent_id.to_string(),
            position,
            batch_id: Some(batch_id),
            sequence_in_batch: Some(idx as i64),
            role,
            content_json,
            content_preview,
            batch_type: Some(BatchType::UserRequest),
            source: Some("letta-import".to_string()),
            source_metadata: None,
            is_archived: msg.in_context == Some(false),
            is_deleted: false,
            created_at: msg.created_at.unwrap_or(now),
        });
    }

    let message_count = converted.len() as u64;

    // Chunk messages by size
    let (cids, blocks) = chunk_messages(converted)?;

    Ok((cids, blocks, message_count))
}

/// Chunk messages into MessageChunk blocks.
fn chunk_messages(
    messages: Vec<MessageExport>,
) -> Result<(Vec<Cid>, Vec<(Cid, Vec<u8>)>), LettaConversionError> {
    use super::estimate_size;

    let mut chunks: Vec<MessageChunk> = Vec::new();
    let mut current_messages: Vec<MessageExport> = Vec::new();
    let mut current_size: usize = 200; // Base overhead
    let mut chunk_index: u32 = 0;

    for msg in messages {
        let msg_size = estimate_size(&msg).unwrap_or(1000);

        if !current_messages.is_empty() && current_size + msg_size > TARGET_CHUNK_BYTES {
            // Flush current chunk
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

    // Flush remaining
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
    }

    // Encode chunks
    let mut cids: Vec<Cid> = Vec::new();
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();

    for chunk in chunks {
        let (cid, data) = encode_block(&chunk, "MessageChunk")
            .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
        cids.push(cid);
        blocks.push((cid, data));
    }

    Ok((cids, blocks))
}

/// Chunk a Loro snapshot into SnapshotChunk blocks.
fn chunk_snapshot(
    snapshot: Vec<u8>,
) -> Result<(Vec<Cid>, Vec<(Cid, Vec<u8>)>), LettaConversionError> {
    if snapshot.len() <= TARGET_CHUNK_BYTES {
        // Single chunk
        let chunk = SnapshotChunk {
            index: 0,
            data: snapshot,
            next_cid: None,
        };
        let (cid, data) = encode_block(&chunk, "SnapshotChunk")
            .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
        return Ok((vec![cid], vec![(cid, data)]));
    }

    // Multiple chunks - build linked list in reverse
    let raw_chunks: Vec<Vec<u8>> = snapshot
        .chunks(TARGET_CHUNK_BYTES)
        .map(|c| c.to_vec())
        .collect();

    let mut cids: Vec<Cid> = Vec::new();
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut next_cid: Option<Cid> = None;

    for (idx, chunk_data) in raw_chunks.iter().enumerate().rev() {
        let chunk = SnapshotChunk {
            index: idx as u32,
            data: chunk_data.clone(),
            next_cid,
        };
        let (cid, data) = encode_block(&chunk, "SnapshotChunk")
            .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
        cids.insert(0, cid);
        blocks.insert(0, (cid, data));
        next_cid = Some(cid);
    }

    Ok((cids, blocks))
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Parse model string like "anthropic/claude-sonnet-4-5-20250929" into (provider, model).
fn parse_model_string(agent: &AgentSchema) -> (String, String) {
    // Try new-style model field first
    if let Some(ref model) = agent.model {
        if let Some((provider, name)) = model.split_once('/') {
            return (provider.to_string(), name.to_string());
        }
        return ("unknown".to_string(), model.clone());
    }

    // Fall back to llm_config
    if let Some(ref config) = agent.llm_config {
        if let Some(ref model) = config.model {
            // Try to infer provider from endpoint_type
            let provider = config
                .model_endpoint_type
                .as_deref()
                .unwrap_or("openai")
                .to_string();
            return (provider, model.clone());
        }
    }

    // Default
    (
        "anthropic".to_string(),
        "claude-sonnet-4-5-20250929".to_string(),
    )
}

/// Build agent config JSON from Letta agent schema.
fn build_agent_config(agent: &AgentSchema) -> serde_json::Value {
    let mut config = serde_json::json!({});

    if let Some(ref llm) = agent.llm_config {
        if let Some(ctx) = llm.context_window {
            config["context_window"] = serde_json::json!(ctx);
        }
        if let Some(temp) = llm.temperature {
            config["temperature"] = serde_json::json!(temp);
        }
        if let Some(max) = llm.max_tokens {
            config["max_tokens"] = serde_json::json!(max);
        }
    }

    if let Some(ref meta) = agent.metadata {
        config["letta_metadata"] = meta.clone();
    }

    config
}

/// Map Letta block label to Pattern block type.
fn label_to_block_type(label: &str) -> MemoryBlockType {
    match label.to_lowercase().as_str() {
        "persona" | "human" | "system" => MemoryBlockType::Core,
        "scratchpad" | "working" | "notes" => MemoryBlockType::Working,
        "archival" | "archive" | "long_term" => MemoryBlockType::Archival,
        _ => MemoryBlockType::Working, // Default to working memory
    }
}

/// Convert plain text to a Loro document snapshot.
fn text_to_loro_snapshot(text: &str) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    let text_container = doc.get_text("content");
    text_container.insert(0, text).unwrap();
    doc.export(loro::ExportMode::Snapshot).unwrap_or_default()
}

/// Write CAR file with manifest and blocks.
async fn write_car_file(
    path: &Path,
    manifest: ExportManifest,
    blocks: Vec<(Cid, Vec<u8>)>,
) -> Result<(), LettaConversionError> {
    use iroh_car::{CarHeader, CarWriter};

    let (manifest_cid, manifest_data) = encode_block(&manifest, "ExportManifest")
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

    let file = File::create(path).await?;
    let header = CarHeader::new_v1(vec![manifest_cid]);
    let mut writer = CarWriter::new(header, file);

    // Write manifest first
    writer
        .write(manifest_cid, &manifest_data)
        .await
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

    // Write all other blocks
    for (cid, data) in blocks {
        writer
            .write(cid, &data)
            .await
            .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;
    }

    writer
        .finish()
        .await
        .map_err(|e| LettaConversionError::Encoding(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_string() {
        let agent = AgentSchema {
            id: "test".to_string(),
            name: None,
            agent_type: None,
            system: None,
            description: None,
            metadata: None,
            memory_blocks: vec![],
            tool_ids: vec![],
            tools: vec![],
            tool_rules: vec![],
            block_ids: vec![],
            include_base_tools: Some(true),
            include_multi_agent_tools: Some(false),
            model: Some("anthropic/claude-sonnet-4-5-20250929".to_string()),
            embedding: None,
            llm_config: None,
            embedding_config: None,
            in_context_message_ids: vec![],
            messages: vec![],
            files_agents: vec![],
            group_ids: vec![],
        };

        let (provider, model) = parse_model_string(&agent);
        assert_eq!(provider, "anthropic");
        assert_eq!(model, "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn test_label_to_block_type() {
        assert!(matches!(
            label_to_block_type("persona"),
            MemoryBlockType::Core
        ));
        assert!(matches!(
            label_to_block_type("human"),
            MemoryBlockType::Core
        ));
        assert!(matches!(
            label_to_block_type("scratchpad"),
            MemoryBlockType::Working
        ));
        assert!(matches!(
            label_to_block_type("archival"),
            MemoryBlockType::Archival
        ));
        assert!(matches!(
            label_to_block_type("random"),
            MemoryBlockType::Working
        ));
    }

    #[test]
    fn test_text_to_loro_snapshot() {
        let snapshot = text_to_loro_snapshot("Hello, world!");
        assert!(!snapshot.is_empty());

        // Verify roundtrip
        let doc = loro::LoroDoc::new();
        doc.import(&snapshot).unwrap();
        let text = doc.get_text("content");
        assert_eq!(text.to_string(), "Hello, world!");
    }
}
