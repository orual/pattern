//! Message value type: a thin wrapper around `genai::chat::ChatMessage` with
//! pattern-specific identity, ownership, ordering, batch membership, and
//! response metadata.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, BatchId, MessageId};
use crate::types::block_ref::BlockRef;
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
