//! Types for agent export/import

use chrono::{DateTime, Utc};
use cid::Cid;
use serde::{Deserialize, Serialize};

use crate::{AgentMemoryRelation, MemoryBlock};

use crate::agent_entity::AgentRecord;
use crate::groups::{
    AgentGroup, AgentType, CompressionStrategy, Constellation, ConstellationMembership,
    GroupMembership,
};
use crate::id::AgentId;
use crate::id::UserId;
use crate::message::{AgentMessageRelation, Message};

/// Manifest describing any export - this is always the root of a CAR file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    /// Export format version
    pub version: u32,

    /// When this export was created
    pub exported_at: DateTime<Utc>,

    /// Type of export
    pub export_type: ExportType,

    /// Export statistics
    pub stats: ExportStats,

    /// CID of the actual export data
    pub data_cid: Cid,
}

/// Type of data being exported
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportType {
    Agent,
    Group,
    Constellation,
}

/// Agent export with all related data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExport {
    /// CID of the slim agent export record
    pub agent_cid: Cid,

    /// CIDs of message chunks
    pub message_chunk_cids: Vec<Cid>,

    /// CIDs of memory chunks
    pub memory_chunk_cids: Vec<Cid>,
}

/// Slim, export-oriented view of an agent. Contains core metadata and
/// references to message/memory chunks by CID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecordExport {
    pub id: AgentId,
    pub name: String,
    pub agent_type: AgentType,

    // Model configuration
    pub model_id: Option<String>,
    pub model_config: std::collections::HashMap<String, serde_json::Value>,

    // Context/configuration parameters
    pub base_instructions: String,
    pub max_messages: usize,
    pub max_message_age_hours: i64,
    pub compression_threshold: usize,
    pub memory_char_limit: usize,
    pub enable_thinking: bool,
    pub compression_strategy: CompressionStrategy,

    // Tool rules
    #[serde(default)]
    pub tool_rules: Vec<crate::config::ToolRuleConfig>,

    // Runtime stats (for reference)
    pub total_messages: usize,
    pub total_tool_calls: usize,
    pub context_rebuilds: usize,
    pub compression_events: usize,

    // Timestamps
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_active: chrono::DateTime<chrono::Utc>,

    // Ownership
    pub owner_id: UserId,

    // Optional summary metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_summary: Option<String>,

    // References to data chunks instead of inline data
    pub message_chunks: Vec<Cid>, // CIDs of MessageChunk blocks
    pub memory_chunks: Vec<Cid>,  // CIDs of MemoryChunk blocks
}

impl AgentRecordExport {
    pub fn from_agent(
        agent: &AgentRecord,
        message_chunks: Vec<Cid>,
        memory_chunks: Vec<Cid>,
    ) -> Self {
        Self {
            id: agent.id.clone(),
            name: agent.name.clone(),
            agent_type: agent.agent_type.clone(),
            model_id: agent.model_id.clone(),
            model_config: agent.model_config.clone(),
            base_instructions: agent.base_instructions.clone(),
            max_messages: agent.max_messages,
            max_message_age_hours: agent.max_message_age_hours,
            compression_threshold: agent.compression_threshold,
            memory_char_limit: agent.memory_char_limit,
            enable_thinking: agent.enable_thinking,
            compression_strategy: agent.compression_strategy.clone(),
            tool_rules: agent.tool_rules.clone(),
            total_messages: agent.total_messages,
            total_tool_calls: agent.total_tool_calls,
            context_rebuilds: agent.context_rebuilds,
            compression_events: agent.compression_events,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
            last_active: agent.last_active,
            owner_id: agent.owner_id.clone(),
            message_summary: agent.message_summary.clone(),
            message_chunks,
            memory_chunks,
        }
    }
}

/// Statistics about an export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportStats {
    /// Number of memory blocks exported
    pub memory_count: u64,

    /// Total number of messages exported
    pub message_count: u64,

    /// Number of message chunks
    pub chunk_count: u64,

    /// Total blocks in the CAR file
    pub total_blocks: u64,

    /// Uncompressed size in bytes
    pub uncompressed_size: u64,

    /// Compressed size if compression was used
    pub compressed_size: Option<u64>,
}

/// A chunk of messages for streaming
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageChunk {
    /// Sequential chunk ID
    pub chunk_id: u32,

    /// Snowflake ID of first message
    pub start_position: String,

    /// Snowflake ID of last message
    pub end_position: String,

    /// Messages in this chunk with their relations (includes position)
    pub messages: Vec<(Message, AgentMessageRelation)>,

    /// CID of next chunk if any
    pub next_chunk: Option<Cid>,
}

/// A chunk of memories for streaming
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    /// Sequential chunk ID
    pub chunk_id: u32,

    /// Memories in this chunk with their agent relations (includes access_level)
    pub memories: Vec<(MemoryBlock, AgentMemoryRelation)>,

    /// CID of next chunk if any
    pub next_chunk: Option<Cid>,
}

/// A complete constellation export with all relationships
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationExport {
    /// The constellation record itself
    pub constellation: Constellation,

    /// All groups in this constellation with their full membership data
    pub groups: Vec<GroupExport>,

    /// CIDs of all agent exports in this constellation
    pub agent_export_cids: Vec<(AgentId, Cid)>,

    /// Membership metadata for direct constellation agents (old AgentId → membership)
    pub agent_memberships: Vec<(AgentId, ConstellationMembership)>,
}

/// A complete group export with all relationships
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExport {
    /// The group record itself
    pub group: AgentGroup,

    /// CIDs of member agent exports (agents are exported separately)
    pub member_agent_cids: Vec<(AgentId, Cid)>,

    /// Membership metadata for group members (old AgentId → membership)
    pub member_memberships: Vec<(AgentId, GroupMembership)>,
}

/// Compression settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct CompressionSettings {
    pub algorithm: String,
    pub level: i32,
}

/// Options for chunking messages
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChunkingStrategy {
    /// Maximum messages per chunk
    pub chunk_size: usize,

    /// Whether to compress individual chunks
    pub compress_chunks: bool,
}
