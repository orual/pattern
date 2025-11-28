//! Message-related models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// A message in an agent's conversation history.
///
/// Messages use Snowflake IDs for absolute ordering across all messages,
/// with batch tracking for atomic request/response cycles.
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

    /// Message content (may be null for tool-only messages)
    pub content: Option<String>,

    /// Tool call ID (for tool messages)
    pub tool_call_id: Option<String>,

    /// Tool name (for tool calls/results)
    pub tool_name: Option<String>,

    /// Tool arguments as JSON (for tool calls)
    pub tool_args: Option<Json<serde_json::Value>>,

    /// Tool result as JSON (for tool responses)
    pub tool_result: Option<Json<serde_json::Value>>,

    /// Source of the message: 'cli', 'discord', 'bluesky', 'api', etc.
    pub source: Option<String>,

    /// Source-specific metadata (channel ID, message ID, etc.)
    pub source_metadata: Option<Json<serde_json::Value>>,

    /// Whether this message has been archived (compressed into a summary)
    pub is_archived: bool,

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

/// An archive summary replacing a range of messages.
///
/// When conversation history grows too long, older messages are compressed
/// into summaries. The original messages are marked as archived but retained
/// for search and history purposes.
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
