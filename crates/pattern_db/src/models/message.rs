//! Message-related models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// A message in an agent's conversation history.
///
/// Messages use Snowflake IDs for absolute ordering across all messages,
/// with batch tracking for atomic request/response cycles.
///
/// The content is stored as JSON to support all MessageContent variants
/// from the domain layer without data loss.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Message {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Snowflake ID as string for sorting (absolute ordering)
    pub position: String,

    /// Groups request/response cycles together
    pub batch_id: Option<String>,

    /// Order within a batch (0 = first message)
    pub sequence_in_batch: Option<i64>,

    /// Message role
    pub role: MessageRole,

    /// Message content stored as JSON to support all variants:
    /// - Text(String)
    /// - Parts(Vec<ContentPart>)
    /// - ToolCalls(Vec<ToolCall>)
    /// - ToolResponses(Vec<ToolResponse>)
    /// - Blocks(Vec<ContentBlock>)
    pub content_json: Json<serde_json::Value>,

    /// Text preview for FTS and quick access (extracted from content_json)
    pub content_preview: Option<String>,

    /// Batch type for categorizing message processing cycles (stored as TEXT in SQLite)
    pub batch_type: Option<BatchType>,

    /// Source of the message: 'cli', 'discord', 'bluesky', 'api', etc.
    pub source: Option<String>,

    /// Source-specific metadata (channel ID, message ID, etc.)
    pub source_metadata: Option<Json<serde_json::Value>>,

    /// Whether this message has been archived (compressed into a summary)
    pub is_archived: bool,

    /// Whether this message has been soft-deleted (tombstone)
    /// Tombstoned messages should be treated as if they don't exist.
    pub is_deleted: bool,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Message roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// User/human message
    User,
    /// Assistant/agent response
    Assistant,
    /// System message (instructions, context)
    System,
    /// Tool call or result
    Tool,
}

impl Default for MessageRole {
    fn default() -> Self {
        Self::User
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::System => write!(f, "system"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

/// Batch type for categorizing message processing cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BatchType {
    /// User-initiated interaction
    UserRequest,
    /// Inter-agent communication
    AgentToAgent,
    /// System-initiated (e.g., scheduled task, sleeptime)
    SystemTrigger,
    /// Continuation of previous batch (for long responses)
    Continuation,
}

/// An archive summary replacing a range of messages.
///
/// When conversation history grows too long, older messages are compressed
/// into summaries. The original messages are marked as archived but retained
/// for search and history purposes.
///
/// Summaries can be chained: when multiple summaries accumulate, they can be
/// summarized again into a higher-level summary (summary of summaries).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ArchiveSummary {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// LLM-generated summary of the archived messages
    pub summary: String,

    /// Starting position (Snowflake ID) of summarized range
    pub start_position: String,

    /// Ending position (Snowflake ID) of summarized range
    pub end_position: String,

    /// Number of messages summarized
    pub message_count: i64,

    /// Previous summary this one extends (for chaining)
    /// When summarizing summaries, this links to the prior summary
    /// that was incorporated into this one.
    pub previous_summary_id: Option<String>,

    /// Depth of summary chain (0 = direct message summary, 1+ = summary of summaries)
    pub depth: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Lightweight message projection for listing/searching.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct MessageSummary {
    /// Message ID
    pub id: String,

    /// Position for ordering
    pub position: String,

    /// Message role
    pub role: MessageRole,

    /// Truncated content preview
    pub content_preview: Option<String>,

    /// Source platform
    pub source: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// A queued message for agent-to-agent communication.
///
/// Used by the MessageRouter to queue messages between agents
/// when the target agent is not immediately available.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Unique identifier
    pub id: String,

    /// Target agent ID
    pub target_agent_id: String,

    /// Source agent ID (if sent by another agent)
    pub source_agent_id: Option<String>,

    /// Message content (display preview, for backwards compat and debugging)
    pub content: String,

    /// JSON serialized MessageOrigin
    pub origin_json: Option<String>,

    /// JSON for extra metadata (legacy field, kept for backwards compat)
    pub metadata_json: Option<String>,

    /// Priority (higher = more urgent)
    pub priority: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Processing timestamp (NULL until processed)
    pub processed_at: Option<DateTime<Utc>>,

    // === New fields for full message preservation ===
    /// Full MessageContent as JSON (Text, Parts, ToolCalls, etc.)
    pub content_json: Option<String>,

    /// Full MessageMetadata as JSON (includes block_refs, user_id, custom, etc.)
    pub metadata_json_full: Option<String>,

    /// Batch ID for notification batching
    pub batch_id: Option<String>,

    /// Message role (user, assistant, system, tool)
    pub role: String,
}
