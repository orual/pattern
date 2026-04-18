//! Message value type: a thin wrapper around `genai::chat::ChatMessage` with
//! pattern-specific identity, ownership, ordering, batch membership, and
//! response metadata.
//!
//! ## Attachments
//!
//! [`MessageAttachment`]s are pattern-level metadata that render as content onto
//! the wire at compose-time but are NOT part of the stored `ChatMessage`
//! structure. This keeps the conversational record clean while the wire still
//! gets ephemeral context reminders (e.g. memory snapshots). Attachments are
//! only set on batch-initiating user messages; other messages carry empty
//! attachment vecs.

use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::memory::BlockType;
use crate::types::block_ref::BlockRef;
use crate::types::ids::{AgentId, BatchId, MessageId};
use genai::ModelIden;
use genai::chat::Usage;

/// A message in the agent's conversation log.
///
/// Wraps a `genai::chat::ChatMessage` (which carries role/content/options) and
/// adds pattern-specific metadata: identity, ownership, global-order timestamp,
/// batch membership, optional per-response metadata for assistant messages,
/// and memory block references to load when this message is in-context.
///
/// Ordering:
/// - Within a batch: by `created_at`.
/// - Across batches: by the first message's `created_at`.
///
/// `created_at` is a `jiff::Timestamp` (nanosecond precision). It doubles as
/// the global monotonic ordering key; replaces the legacy `SnowflakePosition`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub chat_message: genai::chat::ChatMessage,
    pub id: MessageId,
    pub owner_id: AgentId,
    pub created_at: Timestamp,
    pub batch: BatchId,
    /// Populated for assistant messages that originated from a `ChatResponse`.
    /// `None` for user and tool messages.
    pub response_meta: Option<ResponseMeta>,
    /// Memory blocks to load for this message's context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_refs: Vec<BlockRef>,
    /// Pattern-level attachments. Rendered into `ChatMessage.content` at
    /// compose-time. NOT persisted in `ChatMessage` itself — keeps the
    /// conversational record clean. Only set on batch-initiating user
    /// messages; other messages have empty attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
}

/// Pattern-level metadata that renders as content onto the wire at compose-time
/// but is not part of the stored `ChatMessage` structure. Exists so the
/// conversational record stays uncontaminated by ephemeral context reminders,
/// while the wire still receives them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageAttachment {
    /// Memory snapshot attached to a batch-initiating user message
    /// (or to mid-batch tool_result messages when external memory
    /// changes are detected).
    BatchOpeningSnapshot {
        /// Whether this is a full dump or a delta since a prior batch.
        kind: SnapshotKind,
        /// All blocks' labels currently available to this agent. Always
        /// present in both Full and Delta so the model knows the
        /// complete block namespace.
        block_names: Vec<SmolStr>,
        /// For Full: rendered content of ALL blocks.
        /// For Delta: rendered content of blocks that changed since
        /// prior batch.
        blocks: Vec<RenderedBlock>,
        /// For Delta: labels of blocks edited since prior batch. Empty
        /// for Full.
        edited_blocks: Vec<SmolStr>,
    },
}

/// Whether a [`MessageAttachment::BatchOpeningSnapshot`] is a full memory
/// dump or a delta since a prior batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnapshotKind {
    /// Full memory dump. Used for: first batch of session,
    /// post-compaction, periodic refresh every N batches.
    Full,
    /// Delta since prior batch. Used in normal case.
    Delta {
        /// The batch_id that this delta is expressed against.
        since_batch: BatchId,
    },
}

/// A pre-rendered memory block carried inside a [`MessageAttachment`].
/// Contains the label, rendered text, and a content hash for
/// delta-comparison across batches.
///
/// Uses `Arc<str>` for content (O(1) clone since block content can be
/// large) and `SmolStr` for label (inlines short strings). This struct
/// is immutable once constructed -- unlike `StructuredDocument` it does
/// not share a live `LoroDoc` with the memory cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedBlock {
    /// Block label (e.g. "persona", "task_list"). SmolStr for cheap
    /// inline storage of short labels.
    pub label: SmolStr,
    /// Block type at snapshot time.
    pub block_type: BlockType,
    /// Rendered content when this block is meant to be surfaced on the
    /// wire. `None` means "tracked but silent" -- hash is present for
    /// delta detection but wire rendering skips this block.
    /// `Arc<str>` for O(1) clone when present.
    pub rendered: Option<Arc<str>>,
    /// Stable content hash of the rendered text, used for delta
    /// comparison. Computed from the rendered bytes at snapshot time.
    pub content_hash: u64,
}

/// Policy for selecting which blocks appear in a snapshot attachment.
///
/// Applied during both Full and Delta construction. Default includes
/// Core and Working blocks; Archival (searchable on-demand) and Log
/// (high-volume) are excluded.
#[derive(Debug, Clone)]
pub struct SnapshotSelection {
    /// Block types to include. Default: `[Core, Working]`.
    pub include_types: Vec<BlockType>,
    /// Explicit block-label allowlist. If empty, include all blocks
    /// matching `include_types`. If non-empty, restrict to these
    /// labels regardless of type.
    pub include_labels: Vec<SmolStr>,
    /// Explicit label exclusions (applied after include_types /
    /// include_labels). Useful for opting specific blocks out.
    pub exclude_labels: Vec<SmolStr>,
}

impl Default for SnapshotSelection {
    fn default() -> Self {
        Self {
            include_types: vec![BlockType::Core, BlockType::Working],
            include_labels: Vec::new(),
            exclude_labels: Vec::new(),
        }
    }
}

impl SnapshotSelection {
    /// Test whether a block with the given label and type passes the
    /// selection filter.
    pub fn accepts(&self, label: &str, block_type: BlockType) -> bool {
        // Check exclude list first.
        if self.exclude_labels.iter().any(|l| l.as_str() == label) {
            return false;
        }
        // If include_labels is non-empty, restrict to those.
        if !self.include_labels.is_empty() {
            return self.include_labels.iter().any(|l| l.as_str() == label);
        }
        // Otherwise, check include_types.
        self.include_types.contains(&block_type)
    }
}

/// Per-response metadata harvested from `genai::chat::ChatResponse` at the
/// moment the assistant message was constructed.
///
/// One `ChatResponse` per assistant message; a batch may contain many assistant
/// messages (one per tool-call round-trip), each carrying its own metadata.
///
/// Note: genai does not expose a stop reason or response ID in its current
/// API. Fields here reflect what `ChatResponse` actually provides. If genai
/// adds these in future, this type should be updated accordingly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub usage: Usage,
    pub reasoning_content: Option<String>,
    pub model_iden: ModelIden,
    pub provider_model_iden: ModelIden,
}
