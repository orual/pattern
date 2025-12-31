//! Export types for CAR archive format v3.
//!
//! These types are designed for DAG-CBOR serialization and are export-specific
//! variants of the pattern_db models. They avoid storing embeddings and handle
//! large binary data (like Loro snapshots) via chunking.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use cid::Cid;
use serde::{Deserialize, Serialize};

use pattern_db::models::{
    Agent, AgentGroup, AgentStatus, ArchivalEntry, ArchiveSummary, BatchType, GroupMember,
    GroupMemberRole, MemoryBlock, MemoryBlockType, MemoryPermission, Message, MessageRole,
    PatternType,
};

// ============================================================================
// Manifest and Top-Level Types
// ============================================================================

/// Root manifest for any CAR export - always the root block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    /// Export format version (currently 3)
    pub version: u32,

    /// When this export was created
    pub exported_at: DateTime<Utc>,

    /// Type of export (Agent, Group, or Constellation)
    pub export_type: ExportType,

    /// Export statistics
    pub stats: ExportStats,

    /// CID of the actual export data (AgentExport, GroupExport, or ConstellationExport)
    pub data_cid: Cid,
}

/// Type of data being exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportType {
    /// Single agent with all its data
    Agent,
    /// Group with member agents
    Group,
    /// Full constellation with all agents and groups
    Constellation,
}

/// Statistics about an export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportStats {
    /// Number of agents exported
    pub agent_count: u64,

    /// Number of groups exported
    pub group_count: u64,

    /// Total messages exported
    pub message_count: u64,

    /// Total memory blocks exported
    pub memory_block_count: u64,

    /// Total archival entries exported
    pub archival_entry_count: u64,

    /// Total archive summaries exported
    pub archive_summary_count: u64,

    /// Number of message chunks
    pub chunk_count: u64,

    /// Total blocks in the CAR file
    pub total_blocks: u64,

    /// Total bytes (uncompressed)
    pub total_bytes: u64,
}

// ============================================================================
// Agent Export Types
// ============================================================================

/// Complete agent export with references to chunked data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExport {
    /// Agent record (inline - small)
    pub agent: AgentRecord,

    /// CIDs of message chunks
    pub message_chunk_cids: Vec<Cid>,

    /// CIDs of memory block exports
    pub memory_block_cids: Vec<Cid>,

    /// CIDs of archival entry exports
    pub archival_entry_cids: Vec<Cid>,

    /// CIDs of archive summary exports
    pub archive_summary_cids: Vec<Cid>,
}

/// Agent record for export - mirrors pattern_db::Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    /// Unique identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Optional description
    pub description: Option<String>,

    /// Model provider: 'anthropic', 'openai', 'google', etc.
    pub model_provider: String,

    /// Model name: 'claude-3-5-sonnet', 'gpt-4o', etc.
    pub model_name: String,

    /// System prompt / base instructions
    pub system_prompt: String,

    /// Agent configuration as JSON
    pub config: serde_json::Value,

    /// List of enabled tool names
    pub enabled_tools: Vec<String>,

    /// Tool-specific rules as JSON (optional)
    pub tool_rules: Option<serde_json::Value>,

    /// Agent status
    pub status: AgentStatus,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

impl From<Agent> for AgentRecord {
    fn from(agent: Agent) -> Self {
        Self {
            id: agent.id,
            name: agent.name,
            description: agent.description,
            model_provider: agent.model_provider,
            model_name: agent.model_name,
            system_prompt: agent.system_prompt,
            config: agent.config.0,
            enabled_tools: agent.enabled_tools.0,
            tool_rules: agent.tool_rules.map(|j| j.0),
            status: agent.status,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
        }
    }
}

impl From<&Agent> for AgentRecord {
    fn from(agent: &Agent) -> Self {
        Self {
            id: agent.id.clone(),
            name: agent.name.clone(),
            description: agent.description.clone(),
            model_provider: agent.model_provider.clone(),
            model_name: agent.model_name.clone(),
            system_prompt: agent.system_prompt.clone(),
            config: agent.config.0.clone(),
            enabled_tools: agent.enabled_tools.0.clone(),
            tool_rules: agent.tool_rules.as_ref().map(|j| j.0.clone()),
            status: agent.status,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
        }
    }
}

// ============================================================================
// Memory Block Export Types
// ============================================================================

/// Memory block export - excludes loro_snapshot, references chunks instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockExport {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Semantic label: "persona", "human", "scratchpad", etc.
    pub label: String,

    /// Description for the LLM
    pub description: String,

    /// Block type determines context inclusion behavior
    pub block_type: MemoryBlockType,

    /// Character limit for the block
    pub char_limit: i64,

    /// Permission level for this block
    pub permission: MemoryPermission,

    /// Whether this block is pinned
    pub pinned: bool,

    /// Quick content preview without deserializing Loro
    pub content_preview: Option<String>,

    /// Additional metadata
    pub metadata: Option<serde_json::Value>,

    /// Whether this block is active
    pub is_active: bool,

    /// Loro frontier for version tracking (serialized)
    pub frontier: Option<Vec<u8>>,

    /// Last assigned sequence number for updates
    pub last_seq: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,

    /// CIDs of snapshot chunks (for large loro_snapshots)
    pub snapshot_chunk_cids: Vec<Cid>,

    /// Total size of the loro_snapshot in bytes
    pub total_snapshot_bytes: u64,
}

impl MemoryBlockExport {
    /// Create from a MemoryBlock, with snapshot chunk CIDs provided separately.
    pub fn from_memory_block(
        block: &MemoryBlock,
        snapshot_chunk_cids: Vec<Cid>,
        total_snapshot_bytes: u64,
    ) -> Self {
        Self {
            id: block.id.clone(),
            agent_id: block.agent_id.clone(),
            label: block.label.clone(),
            description: block.description.clone(),
            block_type: block.block_type,
            char_limit: block.char_limit,
            permission: block.permission,
            pinned: block.pinned,
            content_preview: block.content_preview.clone(),
            metadata: block.metadata.as_ref().map(|j| j.0.clone()),
            is_active: block.is_active,
            frontier: block.frontier.clone(),
            last_seq: block.last_seq,
            created_at: block.created_at,
            updated_at: block.updated_at,
            snapshot_chunk_cids,
            total_snapshot_bytes,
        }
    }
}

/// A chunk of a Loro snapshot (for large snapshots exceeding block size).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotChunk {
    /// Chunk index (0-based)
    pub index: u32,

    /// Binary data for this chunk (encoded as CBOR bytes, not array)
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,

    /// CID of the next chunk, if any (for streaming reconstruction)
    pub next_cid: Option<Cid>,
}

// ============================================================================
// Archival Entry Export Types
// ============================================================================

/// Archival entry export - mirrors pattern_db::ArchivalEntry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivalEntryExport {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Content of the entry
    pub content: String,

    /// Optional structured metadata
    pub metadata: Option<serde_json::Value>,

    /// For chunked large content
    pub chunk_index: i64,

    /// Links chunks together
    pub parent_entry_id: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl From<ArchivalEntry> for ArchivalEntryExport {
    fn from(entry: ArchivalEntry) -> Self {
        Self {
            id: entry.id,
            agent_id: entry.agent_id,
            content: entry.content,
            metadata: entry.metadata.map(|j| j.0),
            chunk_index: entry.chunk_index,
            parent_entry_id: entry.parent_entry_id,
            created_at: entry.created_at,
        }
    }
}

impl From<&ArchivalEntry> for ArchivalEntryExport {
    fn from(entry: &ArchivalEntry) -> Self {
        Self {
            id: entry.id.clone(),
            agent_id: entry.agent_id.clone(),
            content: entry.content.clone(),
            metadata: entry.metadata.as_ref().map(|j| j.0.clone()),
            chunk_index: entry.chunk_index,
            parent_entry_id: entry.parent_entry_id.clone(),
            created_at: entry.created_at,
        }
    }
}

// ============================================================================
// Message Export Types
// ============================================================================

/// A chunk of messages for streaming export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageChunk {
    /// Sequential chunk index (0-based)
    pub chunk_index: u32,

    /// Snowflake ID of first message in chunk
    pub start_position: String,

    /// Snowflake ID of last message in chunk
    pub end_position: String,

    /// Messages in this chunk
    pub messages: Vec<MessageExport>,

    /// Number of messages in this chunk
    pub message_count: u32,
}

/// Message export - mirrors pattern_db::Message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageExport {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Snowflake ID as string for sorting
    pub position: String,

    /// Groups request/response cycles together
    pub batch_id: Option<String>,

    /// Order within a batch
    pub sequence_in_batch: Option<i64>,

    /// Message role
    pub role: MessageRole,

    /// Message content stored as JSON
    pub content_json: serde_json::Value,

    /// Text preview for quick access
    pub content_preview: Option<String>,

    /// Batch type for categorizing message processing cycles
    pub batch_type: Option<BatchType>,

    /// Source of the message
    pub source: Option<String>,

    /// Source-specific metadata
    pub source_metadata: Option<serde_json::Value>,

    /// Whether this message has been archived
    pub is_archived: bool,

    /// Whether this message has been soft-deleted
    pub is_deleted: bool,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl From<Message> for MessageExport {
    fn from(msg: Message) -> Self {
        Self {
            id: msg.id,
            agent_id: msg.agent_id,
            position: msg.position,
            batch_id: msg.batch_id,
            sequence_in_batch: msg.sequence_in_batch,
            role: msg.role,
            content_json: msg.content_json.0,
            content_preview: msg.content_preview,
            batch_type: msg.batch_type,
            source: msg.source,
            source_metadata: msg.source_metadata.map(|j| j.0),
            is_archived: msg.is_archived,
            is_deleted: msg.is_deleted,
            created_at: msg.created_at,
        }
    }
}

impl From<&Message> for MessageExport {
    fn from(msg: &Message) -> Self {
        Self {
            id: msg.id.clone(),
            agent_id: msg.agent_id.clone(),
            position: msg.position.clone(),
            batch_id: msg.batch_id.clone(),
            sequence_in_batch: msg.sequence_in_batch,
            role: msg.role,
            content_json: msg.content_json.0.clone(),
            content_preview: msg.content_preview.clone(),
            batch_type: msg.batch_type,
            source: msg.source.clone(),
            source_metadata: msg.source_metadata.as_ref().map(|j| j.0.clone()),
            is_archived: msg.is_archived,
            is_deleted: msg.is_deleted,
            created_at: msg.created_at,
        }
    }
}

// ============================================================================
// Archive Summary Export Types
// ============================================================================

/// Archive summary export - mirrors pattern_db::ArchiveSummary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSummaryExport {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// LLM-generated summary
    pub summary: String,

    /// Starting position (Snowflake ID) of summarized range
    pub start_position: String,

    /// Ending position (Snowflake ID) of summarized range
    pub end_position: String,

    /// Number of messages summarized
    pub message_count: i64,

    /// Previous summary this one extends (for chaining)
    pub previous_summary_id: Option<String>,

    /// Depth of summary chain
    pub depth: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl From<ArchiveSummary> for ArchiveSummaryExport {
    fn from(summary: ArchiveSummary) -> Self {
        Self {
            id: summary.id,
            agent_id: summary.agent_id,
            summary: summary.summary,
            start_position: summary.start_position,
            end_position: summary.end_position,
            message_count: summary.message_count,
            previous_summary_id: summary.previous_summary_id,
            depth: summary.depth,
            created_at: summary.created_at,
        }
    }
}

impl From<&ArchiveSummary> for ArchiveSummaryExport {
    fn from(summary: &ArchiveSummary) -> Self {
        Self {
            id: summary.id.clone(),
            agent_id: summary.agent_id.clone(),
            summary: summary.summary.clone(),
            start_position: summary.start_position.clone(),
            end_position: summary.end_position.clone(),
            message_count: summary.message_count,
            previous_summary_id: summary.previous_summary_id.clone(),
            depth: summary.depth,
            created_at: summary.created_at,
        }
    }
}

// ============================================================================
// Group Export Types
// ============================================================================

/// Complete group export with inline agent exports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExport {
    /// Group record
    pub group: GroupRecord,

    /// Group members
    pub members: Vec<GroupMemberExport>,

    /// Full agent exports for all members
    pub agent_exports: Vec<AgentExport>,

    /// CIDs of shared memory blocks
    pub shared_memory_cids: Vec<Cid>,

    /// Shared block attachment records for group members
    pub shared_attachment_exports: Vec<SharedBlockAttachmentExport>,
}

/// Group configuration export (thin variant - no agent data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupConfigExport {
    /// Group record
    pub group: GroupRecord,

    /// Member agent IDs only (no full exports)
    pub member_agent_ids: Vec<String>,
}

/// Group record for export - mirrors pattern_db::AgentGroup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRecord {
    /// Unique identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Optional description
    pub description: Option<String>,

    /// Coordination pattern type
    pub pattern_type: PatternType,

    /// Pattern-specific configuration as JSON
    pub pattern_config: serde_json::Value,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

impl From<AgentGroup> for GroupRecord {
    fn from(group: AgentGroup) -> Self {
        Self {
            id: group.id,
            name: group.name,
            description: group.description,
            pattern_type: group.pattern_type,
            pattern_config: group.pattern_config.0,
            created_at: group.created_at,
            updated_at: group.updated_at,
        }
    }
}

impl From<&AgentGroup> for GroupRecord {
    fn from(group: &AgentGroup) -> Self {
        Self {
            id: group.id.clone(),
            name: group.name.clone(),
            description: group.description.clone(),
            pattern_type: group.pattern_type,
            pattern_config: group.pattern_config.0.clone(),
            created_at: group.created_at,
            updated_at: group.updated_at,
        }
    }
}

/// Group member export - mirrors pattern_db::GroupMember.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberExport {
    /// Group ID
    pub group_id: String,

    /// Agent ID
    pub agent_id: String,

    /// Role within the group
    pub role: Option<GroupMemberRole>,

    /// Capabilities this member provides
    pub capabilities: Vec<String>,

    /// When the agent joined the group
    pub joined_at: DateTime<Utc>,
}

impl From<GroupMember> for GroupMemberExport {
    fn from(member: GroupMember) -> Self {
        Self {
            group_id: member.group_id,
            agent_id: member.agent_id,
            role: member.role.map(|j| j.0),
            capabilities: member.capabilities.0,
            joined_at: member.joined_at,
        }
    }
}

impl From<&GroupMember> for GroupMemberExport {
    fn from(member: &GroupMember) -> Self {
        Self {
            group_id: member.group_id.clone(),
            agent_id: member.agent_id.clone(),
            role: member.role.as_ref().map(|j| j.0.clone()),
            capabilities: member.capabilities.0.clone(),
            joined_at: member.joined_at,
        }
    }
}

// ============================================================================
// Shared Block Attachment Export Types
// ============================================================================

/// Shared block attachment export - records a block being shared with an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedBlockAttachmentExport {
    /// The shared block ID
    pub block_id: String,

    /// Agent gaining access
    pub agent_id: String,

    /// Permission level for this attachment
    pub permission: MemoryPermission,

    /// When the attachment was created
    pub attached_at: DateTime<Utc>,
}

impl From<pattern_db::models::SharedBlockAttachment> for SharedBlockAttachmentExport {
    fn from(attachment: pattern_db::models::SharedBlockAttachment) -> Self {
        Self {
            block_id: attachment.block_id,
            agent_id: attachment.agent_id,
            permission: attachment.permission,
            attached_at: attachment.attached_at,
        }
    }
}

impl From<&pattern_db::models::SharedBlockAttachment> for SharedBlockAttachmentExport {
    fn from(attachment: &pattern_db::models::SharedBlockAttachment) -> Self {
        Self {
            block_id: attachment.block_id.clone(),
            agent_id: attachment.agent_id.clone(),
            permission: attachment.permission,
            attached_at: attachment.attached_at,
        }
    }
}

// ============================================================================
// Constellation Export Types
// ============================================================================

/// Full constellation export with deduplicated agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationExport {
    /// Export format version
    pub version: u32,

    /// Owner user ID
    pub owner_id: String,

    /// When this export was created
    pub exported_at: DateTime<Utc>,

    /// Agent exports keyed by agent ID (shared pool for deduplication)
    pub agent_exports: HashMap<String, Cid>,

    /// Group exports (thin variant with CID references)
    pub group_exports: Vec<GroupExportThin>,

    /// CIDs of standalone agents (not in any group)
    pub standalone_agent_cids: Vec<Cid>,

    /// CIDs of all memory blocks (for blocks not included in agent exports)
    pub all_memory_block_cids: Vec<Cid>,

    /// All shared block attachment records in the constellation
    pub shared_attachments: Vec<SharedBlockAttachmentExport>,
}

/// Thin group export for constellation - references agents by CID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExportThin {
    /// Group record
    pub group: GroupRecord,

    /// Group members
    pub members: Vec<GroupMemberExport>,

    /// CIDs of member agent exports (references into constellation's agent pool)
    pub agent_cids: Vec<Cid>,

    /// CIDs of shared memory blocks
    pub shared_memory_cids: Vec<Cid>,

    /// Shared block attachment records for group members
    pub shared_attachment_exports: Vec<SharedBlockAttachmentExport>,
}

// ============================================================================
// Export/Import Options
// ============================================================================

/// Options for exporting agents, groups, or constellations.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// What to export
    pub target: ExportTarget,

    /// Include message history
    pub include_messages: bool,

    /// Include archival entries
    pub include_archival: bool,

    /// Maximum bytes per chunk (default: TARGET_CHUNK_BYTES)
    pub max_chunk_bytes: usize,

    /// Maximum messages per chunk (default: DEFAULT_MAX_MESSAGES_PER_CHUNK)
    pub max_messages_per_chunk: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            target: ExportTarget::Constellation,
            include_messages: true,
            include_archival: true,
            max_chunk_bytes: super::TARGET_CHUNK_BYTES,
            max_messages_per_chunk: super::DEFAULT_MAX_MESSAGES_PER_CHUNK,
        }
    }
}

/// What to export.
#[derive(Debug, Clone)]
pub enum ExportTarget {
    /// Export a single agent by ID
    Agent(String),

    /// Export a group
    Group {
        /// Group ID
        id: String,
        /// If true, export config only (no agent data)
        thin: bool,
    },

    /// Export the full constellation
    Constellation,
}

/// Options for importing agents, groups, or constellations.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Owner user ID for imported entities
    pub owner_id: String,

    /// Optional rename for the imported entity
    pub rename: Option<String>,

    /// Preserve original IDs (may conflict with existing data)
    pub preserve_ids: bool,

    /// Import message history
    pub include_messages: bool,

    /// Import archival entries
    pub include_archival: bool,
}

impl ImportOptions {
    /// Create new import options with the given owner ID.
    pub fn new(owner_id: impl Into<String>) -> Self {
        Self {
            owner_id: owner_id.into(),
            rename: None,
            preserve_ids: false,
            include_messages: true,
            include_archival: true,
        }
    }

    /// Set the rename option.
    pub fn with_rename(mut self, rename: impl Into<String>) -> Self {
        self.rename = Some(rename.into());
        self
    }

    /// Set whether to preserve original IDs.
    pub fn with_preserve_ids(mut self, preserve: bool) -> Self {
        self.preserve_ids = preserve;
        self
    }

    /// Set whether to include messages.
    pub fn with_messages(mut self, include: bool) -> Self {
        self.include_messages = include;
        self
    }

    /// Set whether to include archival entries.
    pub fn with_archival(mut self, include: bool) -> Self {
        self.include_archival = include;
        self
    }
}
