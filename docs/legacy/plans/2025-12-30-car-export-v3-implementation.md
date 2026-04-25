# CAR Export v3 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement CAR v3 import/export for pattern_core and migration converter in pattern_surreal_compat.

**Architecture:** New export module in pattern_core using existing CAR dependencies (iroh-car, serde_ipld_dagcbor, cid). Types mirror pattern_db models. Converter in pattern_surreal_compat maps old types to new then calls pattern_core export.

**Tech Stack:** Rust, iroh-car, serde_ipld_dagcbor, cid, multihash, pattern_db (sqlx/SQLite)

---

## Task 1: Export Types Module

**Files:**
- Create: `crates/pattern_core/src/export/mod.rs`
- Create: `crates/pattern_core/src/export/types.rs`
- Modify: `crates/pattern_core/src/lib.rs` (add export module)

**Step 1: Create the export module structure**

Create `crates/pattern_core/src/export/mod.rs`:

```rust
//! CAR archive export/import for Pattern agents and constellations.
//!
//! Format version 3 - designed for SQLite-backed architecture.

mod types;
mod exporter;
mod importer;

pub use types::*;
pub use exporter::*;
pub use importer::*;

/// Export format version
pub const EXPORT_VERSION: u32 = 3;

/// Maximum bytes per CAR block (IPLD compatibility)
pub const MAX_BLOCK_BYTES: usize = 1_000_000;

/// Default max messages per chunk
pub const DEFAULT_MAX_MESSAGES_PER_CHUNK: usize = 1000;

/// Target bytes per chunk (leave headroom under MAX_BLOCK_BYTES)
pub const TARGET_CHUNK_BYTES: usize = 900_000;
```

**Step 2: Create export types**

Create `crates/pattern_core/src/export/types.rs`:

```rust
//! Types for CAR v3 export/import.

use chrono::{DateTime, Utc};
use cid::Cid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use pattern_db::models::{
    Agent, AgentGroup, AgentStatus, ArchiveSummary, GroupMember, GroupMemberRole,
    MemoryBlock, MemoryBlockType, MemoryPermission, Message, MessageRole, PatternType,
    ArchivalEntry,
};

// =============================================================================
// Manifest & Stats
// =============================================================================

/// Root manifest for any CAR export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    /// Format version (must be 3+)
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

/// Type of data being exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportType {
    Agent,
    Group,
    Constellation,
}

/// Statistics about an export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportStats {
    pub agent_count: u64,
    pub group_count: u64,
    pub message_count: u64,
    pub memory_block_count: u64,
    pub archival_entry_count: u64,
    pub archive_summary_count: u64,
    pub chunk_count: u64,
    pub total_blocks: u64,
    pub total_bytes: u64,
}

// =============================================================================
// Agent Export
// =============================================================================

/// Complete agent export with all related data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExport {
    /// Agent record (inline, small)
    pub agent: AgentRecord,
    /// CIDs of message chunks
    pub message_chunk_cids: Vec<Cid>,
    /// CIDs of memory block exports
    pub memory_block_cids: Vec<Cid>,
    /// CIDs of archival entries
    pub archival_entry_cids: Vec<Cid>,
    /// CIDs of archive summaries
    pub archive_summary_cids: Vec<Cid>,
}

/// Agent record for export (mirrors pattern_db::Agent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub model_provider: String,
    pub model_name: String,
    pub system_prompt: String,
    pub config: serde_json::Value,
    pub enabled_tools: Vec<String>,
    pub tool_rules: Option<serde_json::Value>,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
            tool_rules: agent.tool_rules.as_ref().map(|r| r.0.clone()),
            status: agent.status,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
        }
    }
}

// =============================================================================
// Memory Block Export
// =============================================================================

/// Memory block export with chunked Loro snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockExport {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: MemoryBlockType,
    pub char_limit: i64,
    pub permission: MemoryPermission,
    pub pinned: bool,
    /// Rendered text content for inspection
    pub content_preview: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: bool,
    pub frontier: Option<Vec<u8>>,
    pub last_seq: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// CIDs of snapshot chunks (if snapshot > 1MB)
    pub snapshot_chunk_cids: Vec<Cid>,
    /// Total bytes of Loro snapshot
    pub total_snapshot_bytes: u64,
}

/// A chunk of Loro snapshot data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotChunk {
    pub index: u32,
    pub data: Vec<u8>,
    /// Optional forward link for streaming
    pub next_cid: Option<Cid>,
}

// =============================================================================
// Archival Entry Export
// =============================================================================

/// Archival entry export (text, reasonable size).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivalEntryExport {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub chunk_index: i64,
    pub parent_entry_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&ArchivalEntry> for ArchivalEntryExport {
    fn from(entry: &ArchivalEntry) -> Self {
        Self {
            id: entry.id.clone(),
            agent_id: entry.agent_id.clone(),
            content: entry.content.clone(),
            metadata: entry.metadata.as_ref().map(|m| m.0.clone()),
            chunk_index: entry.chunk_index,
            parent_entry_id: entry.parent_entry_id.clone(),
            created_at: entry.created_at,
        }
    }
}

// =============================================================================
// Message Export
// =============================================================================

/// A chunk of messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageChunk {
    pub chunk_index: u32,
    /// Snowflake ID of first message
    pub start_position: String,
    /// Snowflake ID of last message
    pub end_position: String,
    /// Messages in this chunk
    pub messages: Vec<MessageExport>,
    pub message_count: u32,
}

/// Message export (mirrors pattern_db::Message).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageExport {
    pub id: String,
    pub agent_id: String,
    pub position: String,
    pub batch_id: Option<String>,
    pub sequence_in_batch: Option<i64>,
    pub role: MessageRole,
    pub content_json: serde_json::Value,
    pub content_preview: Option<String>,
    pub batch_type: Option<pattern_db::models::BatchType>,
    pub source: Option<String>,
    pub source_metadata: Option<serde_json::Value>,
    pub is_archived: bool,
    pub is_deleted: bool,
    pub created_at: DateTime<Utc>,
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
            source_metadata: msg.source_metadata.as_ref().map(|m| m.0.clone()),
            is_archived: msg.is_archived,
            is_deleted: msg.is_deleted,
            created_at: msg.created_at,
        }
    }
}

// =============================================================================
// Archive Summary Export
// =============================================================================

/// Archive summary export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSummaryExport {
    pub id: String,
    pub agent_id: String,
    pub summary: String,
    pub start_position: String,
    pub end_position: String,
    pub message_count: i64,
    pub previous_summary_id: Option<String>,
    pub depth: i64,
    pub created_at: DateTime<Utc>,
}

impl From<&ArchiveSummary> for ArchiveSummaryExport {
    fn from(s: &ArchiveSummary) -> Self {
        Self {
            id: s.id.clone(),
            agent_id: s.agent_id.clone(),
            summary: s.summary.clone(),
            start_position: s.start_position.clone(),
            end_position: s.end_position.clone(),
            message_count: s.message_count,
            previous_summary_id: s.previous_summary_id.clone(),
            depth: s.depth,
            created_at: s.created_at,
        }
    }
}

// =============================================================================
// Group Export
// =============================================================================

/// Group export with inline agents (standalone) or CID refs (in constellation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExport {
    pub group: GroupRecord,
    pub members: Vec<GroupMemberExport>,
    /// Inline agent exports (for standalone group export)
    pub agent_exports: Vec<AgentExport>,
    /// Shared memory block CIDs (group-level blocks)
    pub shared_memory_cids: Vec<Cid>,
}

/// Thin group export (config only, no agent data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupConfigExport {
    pub group: GroupRecord,
    pub member_agent_ids: Vec<String>,
}

/// Group record for export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub pattern_type: PatternType,
    pub pattern_config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

/// Group member export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberExport {
    pub agent_id: String,
    pub role: Option<GroupMemberRole>,
    pub capabilities: Vec<String>,
    pub joined_at: DateTime<Utc>,
}

impl From<&GroupMember> for GroupMemberExport {
    fn from(member: &GroupMember) -> Self {
        Self {
            agent_id: member.agent_id.clone(),
            role: member.role.as_ref().map(|r| r.0.clone()),
            capabilities: member.capabilities.0.clone(),
            joined_at: member.joined_at,
        }
    }
}

// =============================================================================
// Constellation Export
// =============================================================================

/// Constellation export with deduplicated agent pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationExport {
    pub version: u32,
    pub owner_id: String,
    pub exported_at: DateTime<Utc>,
    /// Shared agent pool: agent_id → CID of AgentExport
    pub agent_exports: HashMap<String, Cid>,
    /// Groups referencing agents by CID
    pub group_exports: Vec<GroupExportThin>,
    /// Standalone agents not in any group
    pub standalone_agent_cids: Vec<Cid>,
}

/// Thin group within constellation (agents by CID reference).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExportThin {
    pub group: GroupRecord,
    pub members: Vec<GroupMemberExport>,
    /// CIDs into constellation's agent_exports pool
    pub agent_cids: Vec<Cid>,
    pub shared_memory_cids: Vec<Cid>,
}

// =============================================================================
// Export/Import Options
// =============================================================================

/// Options for exporting.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub target: ExportTarget,
    pub include_messages: bool,
    pub include_archival: bool,
    pub max_chunk_bytes: usize,
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
    Agent(String),
    Group { id: String, thin: bool },
    Constellation,
}

/// Options for importing.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub owner_id: String,
    pub rename: Option<String>,
    pub preserve_ids: bool,
    pub include_messages: bool,
    pub include_archival: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            owner_id: String::new(),
            rename: None,
            preserve_ids: false,
            include_messages: true,
            include_archival: true,
        }
    }
}
```

**Step 3: Add export module to lib.rs**

Modify `crates/pattern_core/src/lib.rs` - add near other module declarations:

```rust
#[cfg(feature = "export")]
pub mod export;
```

**Step 4: Verify compilation**

Run: `cargo check -p pattern-core --features export`
Expected: Compiles successfully (may have unused warnings, that's fine)

**Step 5: Commit**

```bash
git add crates/pattern_core/src/export/ crates/pattern_core/src/lib.rs
git commit -m "feat(export): add CAR v3 export types

- ExportManifest, AgentExport, GroupExport, ConstellationExport
- MemoryBlockExport with chunked Loro snapshots
- MessageChunk with size-based chunking support
- Export/Import options

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 2: CAR Utilities Module

**Files:**
- Create: `crates/pattern_core/src/export/car.rs`
- Modify: `crates/pattern_core/src/export/mod.rs`

**Step 1: Create CAR utilities**

Create `crates/pattern_core/src/export/car.rs`:

```rust
//! CAR file utilities.

use cid::Cid;
use multihash_codetable::{Code, MultihashDigest};
use serde::Serialize;
use serde_ipld_dagcbor::to_vec as encode_dag_cbor;

use crate::error::{CoreError, Result};
use super::MAX_BLOCK_BYTES;

/// DAG-CBOR codec identifier
pub const DAG_CBOR_CODEC: u64 = 0x71;

/// Create a CID from serialized data using Blake3-256.
pub fn create_cid(data: &[u8]) -> Cid {
    let hash = Code::Blake3_256.digest(data);
    Cid::new_v1(DAG_CBOR_CODEC, hash)
}

/// Encode a value to DAG-CBOR and create its CID.
pub fn encode_block<T: Serialize>(value: &T, type_name: &str) -> Result<(Cid, Vec<u8>)> {
    let data = encode_dag_cbor(value).map_err(|e| CoreError::ExportError {
        operation: format!("encoding {}", type_name),
        cause: e.to_string(),
    })?;

    if data.len() > MAX_BLOCK_BYTES {
        return Err(CoreError::ExportError {
            operation: format!("encoding {}", type_name),
            cause: format!("block exceeds {} bytes (got {})", MAX_BLOCK_BYTES, data.len()),
        });
    }

    let cid = create_cid(&data);
    Ok((cid, data))
}

/// Chunk binary data into blocks under the size limit.
pub fn chunk_bytes(data: &[u8], max_chunk_size: usize) -> Vec<Vec<u8>> {
    data.chunks(max_chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Estimate serialized size of a value.
pub fn estimate_size<T: Serialize>(value: &T) -> Result<usize> {
    let data = encode_dag_cbor(value).map_err(|e| CoreError::ExportError {
        operation: "estimating size".to_string(),
        cause: e.to_string(),
    })?;
    Ok(data.len())
}
```

**Step 2: Add to mod.rs**

Add to `crates/pattern_core/src/export/mod.rs`:

```rust
mod car;
pub use car::*;
```

**Step 3: Add ExportError to error.rs**

Modify `crates/pattern_core/src/error.rs` - add variant to CoreError enum:

```rust
    #[error("Export error during {operation}: {cause}")]
    ExportError {
        operation: String,
        cause: String,
    },
```

**Step 4: Verify compilation**

Run: `cargo check -p pattern-core --features export`

**Step 5: Commit**

```bash
git add crates/pattern_core/src/export/car.rs crates/pattern_core/src/export/mod.rs crates/pattern_core/src/error.rs
git commit -m "feat(export): add CAR utilities

- create_cid with Blake3-256 + DAG-CBOR
- encode_block with size validation
- chunk_bytes for large binary data
- estimate_size for chunking decisions

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 3: Agent Exporter

**Files:**
- Create: `crates/pattern_core/src/export/exporter.rs`

**Step 1: Create the exporter with agent export**

Create `crates/pattern_core/src/export/exporter.rs`:

```rust
//! CAR v3 exporter implementation.

use std::collections::HashMap;

use chrono::Utc;
use cid::Cid;
use iroh_car::{CarHeader, CarWriter};
use sqlx::SqlitePool;
use tokio::io::AsyncWrite;

use crate::error::Result;
use pattern_db::queries;

use super::{
    car::{create_cid, encode_block, chunk_bytes},
    types::*,
    EXPORT_VERSION, MAX_BLOCK_BYTES, TARGET_CHUNK_BYTES,
};

/// Exporter for CAR v3 format.
pub struct Exporter {
    pool: SqlitePool,
}

impl Exporter {
    /// Create a new exporter.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Export an agent to a CAR file.
    pub async fn export_agent<W: AsyncWrite + Unpin + Send>(
        &self,
        agent_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();
        let mut collector = BlockCollector::new();

        // Export the agent
        let (agent_export, stats) = self.export_agent_data(agent_id, options, &mut collector).await?;

        // Encode agent export
        let (agent_export_cid, agent_export_data) = encode_block(&agent_export, "AgentExport")?;
        collector.add(agent_export_cid, agent_export_data);

        // Create manifest
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Agent,
            stats,
            data_cid: agent_export_cid,
        };

        // Write CAR file
        self.write_car(output, &manifest, collector).await?;

        Ok(manifest)
    }

    /// Export agent data to blocks.
    async fn export_agent_data(
        &self,
        agent_id: &str,
        options: &ExportOptions,
        collector: &mut BlockCollector,
    ) -> Result<(AgentExport, ExportStats)> {
        let mut stats = ExportStats::default();
        stats.agent_count = 1;

        // Load agent
        let agent = queries::get_agent(&self.pool, agent_id).await?
            .ok_or_else(|| crate::CoreError::ExportError {
                operation: "loading agent".to_string(),
                cause: format!("Agent '{}' not found", agent_id),
            })?;

        let agent_record = AgentRecord::from(&agent);

        // Export memory blocks
        let memory_block_cids = self.export_memory_blocks(agent_id, collector, &mut stats).await?;

        // Export messages
        let message_chunk_cids = if options.include_messages {
            self.export_messages(agent_id, options, collector, &mut stats).await?
        } else {
            vec![]
        };

        // Export archival entries
        let archival_entry_cids = if options.include_archival {
            self.export_archival_entries(agent_id, collector, &mut stats).await?
        } else {
            vec![]
        };

        // Export archive summaries
        let archive_summary_cids = if options.include_messages {
            self.export_archive_summaries(agent_id, collector, &mut stats).await?
        } else {
            vec![]
        };

        let agent_export = AgentExport {
            agent: agent_record,
            message_chunk_cids,
            memory_block_cids,
            archival_entry_cids,
            archive_summary_cids,
        };

        Ok((agent_export, stats))
    }

    /// Export memory blocks with Loro snapshot chunking.
    async fn export_memory_blocks(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let blocks = queries::get_memory_blocks(&self.pool, agent_id).await?;
        let mut cids = Vec::with_capacity(blocks.len());

        for block in &blocks {
            stats.memory_block_count += 1;

            // Chunk the Loro snapshot if needed
            let snapshot_chunks = chunk_bytes(&block.loro_snapshot, TARGET_CHUNK_BYTES);
            let mut snapshot_chunk_cids = Vec::with_capacity(snapshot_chunks.len());

            // Write chunks in reverse to link forward
            let mut next_cid: Option<Cid> = None;
            for (i, chunk_data) in snapshot_chunks.iter().enumerate().rev() {
                let chunk = SnapshotChunk {
                    index: i as u32,
                    data: chunk_data.clone(),
                    next_cid,
                };
                let (cid, data) = encode_block(&chunk, "SnapshotChunk")?;
                collector.add(cid, data);
                snapshot_chunk_cids.push(cid);
                next_cid = Some(cid);
                stats.chunk_count += 1;
            }
            snapshot_chunk_cids.reverse();

            // Create memory block export
            let export = MemoryBlockExport {
                id: block.id.clone(),
                agent_id: block.agent_id.clone(),
                label: block.label.clone(),
                description: block.description.clone(),
                block_type: block.block_type,
                char_limit: block.char_limit,
                permission: block.permission,
                pinned: block.pinned,
                content_preview: block.content_preview.clone(),
                metadata: block.metadata.as_ref().map(|m| m.0.clone()),
                is_active: block.is_active,
                frontier: block.frontier.clone(),
                last_seq: block.last_seq,
                created_at: block.created_at,
                updated_at: block.updated_at,
                snapshot_chunk_cids,
                total_snapshot_bytes: block.loro_snapshot.len() as u64,
            };

            let (cid, data) = encode_block(&export, "MemoryBlockExport")?;
            collector.add(cid, data);
            cids.push(cid);
        }

        Ok(cids)
    }

    /// Export messages in size-based chunks.
    async fn export_messages(
        &self,
        agent_id: &str,
        options: &ExportOptions,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let messages = queries::get_messages(&self.pool, agent_id, None).await?;
        if messages.is_empty() {
            return Ok(vec![]);
        }

        // Chunk messages by size
        let mut chunks: Vec<Vec<MessageExport>> = vec![];
        let mut current_chunk: Vec<MessageExport> = vec![];
        let mut current_size = 0usize;

        for msg in &messages {
            let export = MessageExport::from(msg);
            let msg_size = super::car::estimate_size(&export)?;

            // Check if adding this message would exceed limits
            if !current_chunk.is_empty() &&
               (current_size + msg_size > options.max_chunk_bytes ||
                current_chunk.len() >= options.max_messages_per_chunk)
            {
                chunks.push(std::mem::take(&mut current_chunk));
                current_size = 0;
            }

            current_chunk.push(export);
            current_size += msg_size;
            stats.message_count += 1;
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        // Write chunks and collect CIDs
        let mut cids = Vec::with_capacity(chunks.len());
        for (i, chunk_messages) in chunks.into_iter().enumerate() {
            let chunk = MessageChunk {
                chunk_index: i as u32,
                start_position: chunk_messages.first().map(|m| m.position.clone()).unwrap_or_default(),
                end_position: chunk_messages.last().map(|m| m.position.clone()).unwrap_or_default(),
                message_count: chunk_messages.len() as u32,
                messages: chunk_messages,
            };

            let (cid, data) = encode_block(&chunk, "MessageChunk")?;
            collector.add(cid, data);
            cids.push(cid);
            stats.chunk_count += 1;
        }

        Ok(cids)
    }

    /// Export archival entries.
    async fn export_archival_entries(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let entries = queries::get_archival_entries(&self.pool, agent_id).await?;
        let mut cids = Vec::with_capacity(entries.len());

        for entry in &entries {
            let export = ArchivalEntryExport::from(entry);
            let (cid, data) = encode_block(&export, "ArchivalEntryExport")?;
            collector.add(cid, data);
            cids.push(cid);
            stats.archival_entry_count += 1;
        }

        Ok(cids)
    }

    /// Export archive summaries.
    async fn export_archive_summaries(
        &self,
        agent_id: &str,
        collector: &mut BlockCollector,
        stats: &mut ExportStats,
    ) -> Result<Vec<Cid>> {
        let summaries = queries::get_archive_summaries(&self.pool, agent_id).await?;
        let mut cids = Vec::with_capacity(summaries.len());

        for summary in &summaries {
            let export = ArchiveSummaryExport::from(summary);
            let (cid, data) = encode_block(&export, "ArchiveSummaryExport")?;
            collector.add(cid, data);
            cids.push(cid);
            stats.archive_summary_count += 1;
        }

        Ok(cids)
    }

    /// Write collected blocks to a CAR file.
    async fn write_car<W: AsyncWrite + Unpin + Send>(
        &self,
        mut output: W,
        manifest: &ExportManifest,
        collector: BlockCollector,
    ) -> Result<()> {
        // Encode manifest
        let (manifest_cid, manifest_data) = encode_block(manifest, "ExportManifest")?;

        // Create CAR with manifest as root
        let header = CarHeader::new_v1(vec![manifest_cid]);
        let mut writer = CarWriter::new(header, &mut output);

        // Write manifest first
        writer.write(manifest_cid, &manifest_data).await.map_err(|e| {
            crate::CoreError::ExportError {
                operation: "writing manifest".to_string(),
                cause: e.to_string(),
            }
        })?;

        // Write all collected blocks
        for (cid, data) in collector.blocks {
            writer.write(cid, &data).await.map_err(|e| {
                crate::CoreError::ExportError {
                    operation: "writing block".to_string(),
                    cause: e.to_string(),
                }
            })?;
        }

        writer.finish().await.map_err(|e| {
            crate::CoreError::ExportError {
                operation: "finishing CAR".to_string(),
                cause: e.to_string(),
            }
        })?;

        Ok(())
    }
}

/// Collects blocks during export.
struct BlockCollector {
    blocks: Vec<(Cid, Vec<u8>)>,
}

impl BlockCollector {
    fn new() -> Self {
        Self { blocks: vec![] }
    }

    fn add(&mut self, cid: Cid, data: Vec<u8>) {
        self.blocks.push((cid, data));
    }
}
```

**Step 2: Verify needed query functions exist**

Check if `queries::get_agent`, `queries::get_memory_blocks`, `queries::get_messages`, `queries::get_archival_entries`, `queries::get_archive_summaries` exist in pattern_db. If not, we'll need to add them in Task 4.

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core --features export`

Note: This will likely fail due to missing query functions. That's expected - we'll add them in Task 4.

**Step 4: Commit (even if incomplete)**

```bash
git add crates/pattern_core/src/export/exporter.rs
git commit -m "feat(export): add agent exporter (WIP)

- Exporter struct with export_agent method
- Memory block export with Loro snapshot chunking
- Message export with size-based chunking
- Archival entry and archive summary export
- BlockCollector for CAR writing

Note: Depends on query functions to be added to pattern_db

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 4: Add Missing Query Functions to pattern_db

**Files:**
- Modify: `crates/pattern_db/src/queries/agent.rs`
- Modify: `crates/pattern_db/src/queries/memory.rs`
- Modify: `crates/pattern_db/src/queries/message.rs`

Review existing query functions and add any missing ones needed by the exporter:
- `get_agent(pool, agent_id) -> Option<Agent>`
- `get_memory_blocks(pool, agent_id) -> Vec<MemoryBlock>`
- `get_messages(pool, agent_id, since: Option<DateTime>) -> Vec<Message>`
- `get_archival_entries(pool, agent_id) -> Vec<ArchivalEntry>`
- `get_archive_summaries(pool, agent_id) -> Vec<ArchiveSummary>`

**Step 1: Check existing queries**

Read each query module to see what exists.

**Step 2: Add missing functions**

Add any missing query functions using sqlx macros.

**Step 3: Run sqlx prepare**

```bash
cd crates/pattern_db
sqlx prepare
```

**Step 4: Verify compilation**

```bash
cargo check -p pattern-db
cargo check -p pattern-core --features export
```

**Step 5: Commit**

```bash
git add crates/pattern_db/
git commit -m "feat(db): add export query functions

- get_agent, get_memory_blocks, get_messages
- get_archival_entries, get_archive_summaries
- All queries for CAR export support

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 5: Group and Constellation Export

**Files:**
- Modify: `crates/pattern_core/src/export/exporter.rs`

**Step 1: Add group export method**

Add to Exporter impl:

```rust
    /// Export a group to a CAR file.
    pub async fn export_group<W: AsyncWrite + Unpin + Send>(
        &self,
        group_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();
        let mut collector = BlockCollector::new();
        let mut stats = ExportStats::default();

        // Load group
        let group = queries::get_group(&self.pool, group_id).await?
            .ok_or_else(|| crate::CoreError::ExportError {
                operation: "loading group".to_string(),
                cause: format!("Group '{}' not found", group_id),
            })?;

        let group_record = GroupRecord::from(&group);

        // Load members
        let members = queries::get_group_members(&self.pool, group_id).await?;
        let member_exports: Vec<GroupMemberExport> = members.iter().map(GroupMemberExport::from).collect();

        // Check if thin export
        let thin = matches!(&options.target, ExportTarget::Group { thin: true, .. });

        if thin {
            // Thin export - just config
            let export = GroupConfigExport {
                group: group_record,
                member_agent_ids: members.iter().map(|m| m.agent_id.clone()).collect(),
            };

            let (cid, data) = encode_block(&export, "GroupConfigExport")?;
            collector.add(cid, data);
            stats.group_count = 1;

            let manifest = ExportManifest {
                version: EXPORT_VERSION,
                exported_at: start_time,
                export_type: ExportType::Group,
                stats,
                data_cid: cid,
            };

            self.write_car(output, &manifest, collector).await?;
            return Ok(manifest);
        }

        // Full export - include agent data
        let mut agent_exports = Vec::with_capacity(members.len());
        for member in &members {
            let (agent_export, agent_stats) = self.export_agent_data(&member.agent_id, options, &mut collector).await?;
            stats.agent_count += agent_stats.agent_count;
            stats.message_count += agent_stats.message_count;
            stats.memory_block_count += agent_stats.memory_block_count;
            stats.archival_entry_count += agent_stats.archival_entry_count;
            stats.archive_summary_count += agent_stats.archive_summary_count;
            stats.chunk_count += agent_stats.chunk_count;
            agent_exports.push(agent_export);
        }

        stats.group_count = 1;

        let group_export = GroupExport {
            group: group_record,
            members: member_exports,
            agent_exports,
            shared_memory_cids: vec![], // TODO: shared memory blocks
        };

        let (cid, data) = encode_block(&group_export, "GroupExport")?;
        collector.add(cid, data);

        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Group,
            stats,
            data_cid: cid,
        };

        self.write_car(output, &manifest, collector).await?;
        Ok(manifest)
    }

    /// Export entire constellation to a CAR file.
    pub async fn export_constellation<W: AsyncWrite + Unpin + Send>(
        &self,
        owner_id: &str,
        output: W,
        options: &ExportOptions,
    ) -> Result<ExportManifest> {
        let start_time = Utc::now();
        let mut collector = BlockCollector::new();
        let mut stats = ExportStats::default();

        // Load all agents
        let agents = queries::list_agents(&self.pool).await?;
        let mut agent_cid_map: HashMap<String, Cid> = HashMap::new();

        for agent in &agents {
            let (agent_export, agent_stats) = self.export_agent_data(&agent.id, options, &mut collector).await?;

            let (cid, data) = encode_block(&agent_export, "AgentExport")?;
            collector.add(cid, data);
            agent_cid_map.insert(agent.id.clone(), cid);

            stats.agent_count += 1;
            stats.message_count += agent_stats.message_count;
            stats.memory_block_count += agent_stats.memory_block_count;
            stats.archival_entry_count += agent_stats.archival_entry_count;
            stats.archive_summary_count += agent_stats.archive_summary_count;
            stats.chunk_count += agent_stats.chunk_count;
        }

        // Load all groups
        let groups = queries::list_groups(&self.pool).await?;
        let mut group_exports = Vec::with_capacity(groups.len());

        for group in &groups {
            let members = queries::get_group_members(&self.pool, &group.id).await?;
            let member_exports: Vec<GroupMemberExport> = members.iter().map(GroupMemberExport::from).collect();

            let agent_cids: Vec<Cid> = members.iter()
                .filter_map(|m| agent_cid_map.get(&m.agent_id).copied())
                .collect();

            group_exports.push(GroupExportThin {
                group: GroupRecord::from(group),
                members: member_exports,
                agent_cids,
                shared_memory_cids: vec![],
            });

            stats.group_count += 1;
        }

        // Find standalone agents (not in any group)
        let agents_in_groups: std::collections::HashSet<String> = groups.iter()
            .flat_map(|g| queries::get_group_members(&self.pool, &g.id))
            .flatten()
            .map(|m| m.agent_id.clone())
            .collect();
        // Note: This is simplified - in real impl we'd await properly

        let standalone_agent_cids: Vec<Cid> = agents.iter()
            .filter(|a| !agents_in_groups.contains(&a.id))
            .filter_map(|a| agent_cid_map.get(&a.id).copied())
            .collect();

        let constellation_export = ConstellationExport {
            version: EXPORT_VERSION,
            owner_id: owner_id.to_string(),
            exported_at: start_time,
            agent_exports: agent_cid_map,
            group_exports,
            standalone_agent_cids,
        };

        let (cid, data) = encode_block(&constellation_export, "ConstellationExport")?;
        collector.add(cid, data);

        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: start_time,
            export_type: ExportType::Constellation,
            stats,
            data_cid: cid,
        };

        self.write_car(output, &manifest, collector).await?;
        Ok(manifest)
    }
```

**Step 2: Add missing group query functions to pattern_db if needed**

**Step 3: Verify compilation**

**Step 4: Commit**

---

## Task 6: Importer

**Files:**
- Create: `crates/pattern_core/src/export/importer.rs`

**Step 1: Create importer with agent import**

Create `crates/pattern_core/src/export/importer.rs`:

```rust
//! CAR v3 importer implementation.

use std::collections::HashMap;

use cid::Cid;
use iroh_car::CarReader;
use sqlx::SqlitePool;
use tokio::io::AsyncRead;

use crate::error::Result;
use pattern_db::queries;

use super::{
    types::*,
    EXPORT_VERSION,
};

/// Importer for CAR v3 format.
pub struct Importer {
    pool: SqlitePool,
}

impl Importer {
    /// Create a new importer.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Import from a CAR file.
    pub async fn import<R: AsyncRead + Unpin + Send>(
        &self,
        input: R,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        // Read CAR file
        let mut reader = CarReader::new(input).await.map_err(|e| {
            crate::CoreError::ExportError {
                operation: "reading CAR header".to_string(),
                cause: e.to_string(),
            }
        })?;

        // Load all blocks into memory
        let mut blocks: HashMap<Cid, Vec<u8>> = HashMap::new();
        let roots = reader.header().roots().to_vec();

        while let Some((cid, data)) = reader.next_block().await.map_err(|e| {
            crate::CoreError::ExportError {
                operation: "reading CAR block".to_string(),
                cause: e.to_string(),
            }
        })? {
            blocks.insert(cid, data);
        }

        // Get manifest from root
        let manifest_cid = roots.first().ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading manifest".to_string(),
                cause: "No root CID in CAR file".to_string(),
            }
        })?;

        let manifest_data = blocks.get(manifest_cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading manifest".to_string(),
                cause: "Manifest block not found".to_string(),
            }
        })?;

        let manifest: ExportManifest = serde_ipld_dagcbor::from_slice(manifest_data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding manifest".to_string(),
                cause: e.to_string(),
            }
        })?;

        // Validate version
        if manifest.version < 3 {
            return Err(crate::CoreError::ExportError {
                operation: "validating version".to_string(),
                cause: format!(
                    "CAR version {} is not supported. Use the converter utility to upgrade from v1/v2.",
                    manifest.version
                ),
            });
        }

        // Import based on type
        match manifest.export_type {
            ExportType::Agent => self.import_agent(&blocks, &manifest, options).await,
            ExportType::Group => self.import_group(&blocks, &manifest, options).await,
            ExportType::Constellation => self.import_constellation(&blocks, &manifest, options).await,
        }
    }

    async fn import_agent(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        manifest: &ExportManifest,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        let data = blocks.get(&manifest.data_cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading agent export".to_string(),
                cause: "Agent export block not found".to_string(),
            }
        })?;

        let agent_export: AgentExport = serde_ipld_dagcbor::from_slice(data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding agent export".to_string(),
                cause: e.to_string(),
            }
        })?;

        // Create agent
        let agent_id = self.import_agent_record(&agent_export.agent, options).await?;

        // Import memory blocks
        for cid in &agent_export.memory_block_cids {
            self.import_memory_block(blocks, cid, &agent_id, options).await?;
        }

        // Import messages
        if options.include_messages {
            for cid in &agent_export.message_chunk_cids {
                self.import_message_chunk(blocks, cid, &agent_id).await?;
            }
        }

        // Import archival entries
        if options.include_archival {
            for cid in &agent_export.archival_entry_cids {
                self.import_archival_entry(blocks, cid, &agent_id).await?;
            }
        }

        // Import archive summaries
        if options.include_messages {
            for cid in &agent_export.archive_summary_cids {
                self.import_archive_summary(blocks, cid, &agent_id).await?;
            }
        }

        Ok(ImportResult {
            agent_ids: vec![agent_id],
            group_ids: vec![],
        })
    }

    async fn import_agent_record(
        &self,
        record: &AgentRecord,
        options: &ImportOptions,
    ) -> Result<String> {
        let id = if options.preserve_ids {
            record.id.clone()
        } else {
            uuid::Uuid::new_v4().to_string()
        };

        let name = options.rename.clone().unwrap_or_else(|| record.name.clone());

        // Insert agent
        queries::insert_agent(
            &self.pool,
            &id,
            &name,
            record.description.as_deref(),
            &record.model_provider,
            &record.model_name,
            &record.system_prompt,
            &record.config,
            &record.enabled_tools,
            record.tool_rules.as_ref(),
        ).await?;

        Ok(id)
    }

    async fn import_memory_block(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        cid: &Cid,
        agent_id: &str,
        options: &ImportOptions,
    ) -> Result<()> {
        let data = blocks.get(cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading memory block".to_string(),
                cause: format!("Memory block {} not found", cid),
            }
        })?;

        let export: MemoryBlockExport = serde_ipld_dagcbor::from_slice(data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding memory block".to_string(),
                cause: e.to_string(),
            }
        })?;

        // Reconstruct Loro snapshot from chunks
        let mut snapshot = Vec::with_capacity(export.total_snapshot_bytes as usize);
        for chunk_cid in &export.snapshot_chunk_cids {
            let chunk_data = blocks.get(chunk_cid).ok_or_else(|| {
                crate::CoreError::ExportError {
                    operation: "reading snapshot chunk".to_string(),
                    cause: format!("Snapshot chunk {} not found", chunk_cid),
                }
            })?;

            let chunk: SnapshotChunk = serde_ipld_dagcbor::from_slice(chunk_data).map_err(|e| {
                crate::CoreError::ExportError {
                    operation: "decoding snapshot chunk".to_string(),
                    cause: e.to_string(),
                }
            })?;

            snapshot.extend_from_slice(&chunk.data);
        }

        let id = if options.preserve_ids {
            export.id.clone()
        } else {
            uuid::Uuid::new_v4().to_string()
        };

        // Insert memory block
        queries::insert_memory_block(
            &self.pool,
            &id,
            agent_id,
            &export.label,
            &export.description,
            export.block_type,
            export.char_limit,
            export.permission,
            export.pinned,
            &snapshot,
            export.content_preview.as_deref(),
            export.metadata.as_ref(),
        ).await?;

        Ok(())
    }

    async fn import_message_chunk(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        cid: &Cid,
        agent_id: &str,
    ) -> Result<()> {
        let data = blocks.get(cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading message chunk".to_string(),
                cause: format!("Message chunk {} not found", cid),
            }
        })?;

        let chunk: MessageChunk = serde_ipld_dagcbor::from_slice(data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding message chunk".to_string(),
                cause: e.to_string(),
            }
        })?;

        for msg in &chunk.messages {
            queries::insert_message(
                &self.pool,
                &msg.id,
                agent_id,
                &msg.position,
                msg.batch_id.as_deref(),
                msg.sequence_in_batch,
                msg.role,
                &msg.content_json,
                msg.content_preview.as_deref(),
                msg.batch_type,
                msg.source.as_deref(),
                msg.source_metadata.as_ref(),
                msg.is_archived,
                msg.is_deleted,
            ).await?;
        }

        Ok(())
    }

    async fn import_archival_entry(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        cid: &Cid,
        agent_id: &str,
    ) -> Result<()> {
        let data = blocks.get(cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading archival entry".to_string(),
                cause: format!("Archival entry {} not found", cid),
            }
        })?;

        let entry: ArchivalEntryExport = serde_ipld_dagcbor::from_slice(data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding archival entry".to_string(),
                cause: e.to_string(),
            }
        })?;

        queries::insert_archival_entry(
            &self.pool,
            &entry.id,
            agent_id,
            &entry.content,
            entry.metadata.as_ref(),
            entry.chunk_index,
            entry.parent_entry_id.as_deref(),
        ).await?;

        Ok(())
    }

    async fn import_archive_summary(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        cid: &Cid,
        agent_id: &str,
    ) -> Result<()> {
        let data = blocks.get(cid).ok_or_else(|| {
            crate::CoreError::ExportError {
                operation: "reading archive summary".to_string(),
                cause: format!("Archive summary {} not found", cid),
            }
        })?;

        let summary: ArchiveSummaryExport = serde_ipld_dagcbor::from_slice(data).map_err(|e| {
            crate::CoreError::ExportError {
                operation: "decoding archive summary".to_string(),
                cause: e.to_string(),
            }
        })?;

        queries::insert_archive_summary(
            &self.pool,
            &summary.id,
            agent_id,
            &summary.summary,
            &summary.start_position,
            &summary.end_position,
            summary.message_count,
            summary.previous_summary_id.as_deref(),
            summary.depth,
        ).await?;

        Ok(())
    }

    async fn import_group(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        manifest: &ExportManifest,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        // TODO: Implement group import
        todo!("Group import not yet implemented")
    }

    async fn import_constellation(
        &self,
        blocks: &HashMap<Cid, Vec<u8>>,
        manifest: &ExportManifest,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        // TODO: Implement constellation import
        todo!("Constellation import not yet implemented")
    }
}

/// Result of an import operation.
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub agent_ids: Vec<String>,
    pub group_ids: Vec<String>,
}
```

**Step 2: Add to mod.rs**

**Step 3: Add insert query functions to pattern_db**

**Step 4: Verify compilation**

**Step 5: Commit**

---

## Task 7: CLI Commands

**Files:**
- Modify: `crates/pattern_cli/src/commands/mod.rs` (or appropriate location)

Add export/import subcommands using clap.

---

## Task 8: Converter in pattern_surreal_compat

**Files:**
- Create: `crates/pattern_surreal_compat/src/convert.rs`
- Modify: `crates/pattern_surreal_compat/Cargo.toml` (add pattern-core dependency)

Create converter that:
1. Reads v1/v2 CAR or SurrealDB
2. Maps old types to pattern_db types
3. Calls pattern_core::export to write v3 CAR

---

## Task 9: Integration Tests

**Files:**
- Create: `crates/pattern_core/src/export/tests.rs`

Test roundtrip: export → import → verify data matches.

---

## Task 10: Final Verification

Run full test suite and verify all features work together.

```bash
just pre-commit-all
```

---

**Plan complete and saved to `docs/plans/2025-12-30-car-export-v3-implementation.md`. Two execution options:**

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?**
