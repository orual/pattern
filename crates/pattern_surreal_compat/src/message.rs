//! Message types for Pattern's multi-agent system
//!
//! This module contains the core message types used for agent communication,
//! including support for text, tool calls, tool responses, and multi-modal content.
//! These types are designed for SurrealDB export/import compatibility.

use chrono::{DateTime, Utc};
use pattern_macros::Entity;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::AgentId;
use crate::groups::{SnowflakePosition, get_next_message_position_sync};
use crate::id::{MessageId, RelationId, UserId};

/// Type of processing batch a message belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// A message to be processed by an agent
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "msg")]
pub struct Message {
    pub id: MessageId,
    pub role: ChatRole,

    /// The user (human) who initiated this conversation
    /// This helps track message ownership without tying messages to specific agents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<UserId>,

    /// Message content stored as flexible object for searchability
    pub content: MessageContent,

    /// Metadata stored as flexible object
    pub metadata: MessageMetadata,

    /// Options stored as flexible object
    pub options: MessageOptions,

    // Precomputed fields for performance
    pub has_tool_calls: bool,
    pub word_count: u32,
    pub created_at: DateTime<Utc>,

    // Batch tracking fields (Option during migration, required after)
    /// Unique snowflake ID for absolute ordering
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<SnowflakePosition>,

    /// ID of the first message in this processing batch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<SnowflakePosition>,

    /// Position within the batch (0 for first message)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_num: Option<u32>,

    /// Type of processing cycle this batch represents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_type: Option<BatchType>,

    // Embeddings - loaded selectively via custom methods
    #[serde(
        deserialize_with = "crate::memory::deserialize_f32_vec_flexible",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub embedding: Option<Vec<f32>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
}

/// Metadata associated with a message
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct MessageMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    #[serde(flatten)]
    pub custom: serde_json::Value,
}

/// Message options
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MessageOptions {
    pub cache_control: Option<CacheControl>,
}

/// Cache control options
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum CacheControl {
    Ephemeral,
}

impl From<CacheControl> for MessageOptions {
    fn from(cache_control: CacheControl) -> Self {
        Self {
            cache_control: Some(cache_control),
        }
    }
}

/// Chat roles
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatRole::System => write!(f, "system"),
            ChatRole::User => write!(f, "user"),
            ChatRole::Assistant => write!(f, "assistant"),
            ChatRole::Tool => write!(f, "tool"),
        }
    }
}

impl ChatRole {
    /// Check if this is a System role
    pub fn is_system(&self) -> bool {
        matches!(self, ChatRole::System)
    }

    /// Check if this is a User role
    pub fn is_user(&self) -> bool {
        matches!(self, ChatRole::User)
    }

    /// Check if this is an Assistant role
    pub fn is_assistant(&self) -> bool {
        matches!(self, ChatRole::Assistant)
    }

    /// Check if this is a Tool role
    pub fn is_tool(&self) -> bool {
        matches!(self, ChatRole::Tool)
    }
}

/// Message content variants
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum MessageContent {
    /// Simple text content
    Text(String),

    /// Multi-part content (text + images)
    Parts(Vec<ContentPart>),

    /// Tool calls from the assistant
    ToolCalls(Vec<ToolCall>),

    /// Tool responses
    ToolResponses(Vec<ToolResponse>),

    /// Content blocks - for providers that need exact block sequence preservation (e.g. Anthropic with thinking)
    Blocks(Vec<ContentBlock>),
}

/// Constructors
impl MessageContent {
    /// Create text content
    pub fn from_text(content: impl Into<String>) -> Self {
        MessageContent::Text(content.into())
    }

    /// Create multi-part content
    pub fn from_parts(parts: impl Into<Vec<ContentPart>>) -> Self {
        MessageContent::Parts(parts.into())
    }

    /// Create tool calls content
    pub fn from_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        MessageContent::ToolCalls(tool_calls)
    }
}

/// Getters
impl MessageContent {
    /// Get text content if this is a Text variant
    pub fn text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(content) => Some(content.as_str()),
            _ => None,
        }
    }

    /// Consume and return text content if this is a Text variant
    pub fn into_text(self) -> Option<String> {
        match self {
            MessageContent::Text(content) => Some(content),
            _ => None,
        }
    }

    /// Get tool calls if this is a ToolCalls variant
    pub fn tool_calls(&self) -> Option<&[ToolCall]> {
        match self {
            MessageContent::ToolCalls(calls) => Some(calls),
            _ => None,
        }
    }

    /// Check if content is empty
    pub fn is_empty(&self) -> bool {
        match self {
            MessageContent::Text(content) => content.is_empty(),
            MessageContent::Parts(parts) => parts.is_empty(),
            MessageContent::ToolCalls(calls) => calls.is_empty(),
            MessageContent::ToolResponses(responses) => responses.is_empty(),
            MessageContent::Blocks(blocks) => blocks.is_empty(),
        }
    }
}

// From impls for convenience
impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        MessageContent::Text(s.to_string())
    }
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        MessageContent::Text(s)
    }
}

impl From<&String> for MessageContent {
    fn from(s: &String) -> Self {
        MessageContent::Text(s.clone())
    }
}

impl From<Vec<ToolCall>> for MessageContent {
    fn from(calls: Vec<ToolCall>) -> Self {
        MessageContent::ToolCalls(calls)
    }
}

impl From<ToolResponse> for MessageContent {
    fn from(response: ToolResponse) -> Self {
        MessageContent::ToolResponses(vec![response])
    }
}

impl From<Vec<ContentPart>> for MessageContent {
    fn from(parts: Vec<ContentPart>) -> Self {
        MessageContent::Parts(parts)
    }
}

/// Content part for multi-modal messages
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ContentPart {
    Text(String),
    Image {
        content_type: String,
        source: ImageSource,
    },
}

impl ContentPart {
    /// Create text part
    pub fn from_text(text: impl Into<String>) -> Self {
        ContentPart::Text(text.into())
    }

    /// Create image part from base64
    pub fn from_image_base64(
        content_type: impl Into<String>,
        content: impl Into<Arc<str>>,
    ) -> Self {
        ContentPart::Image {
            content_type: content_type.into(),
            source: ImageSource::Base64(content.into()),
        }
    }

    /// Create image part from URL
    pub fn from_image_url(content_type: impl Into<String>, url: impl Into<String>) -> Self {
        ContentPart::Image {
            content_type: content_type.into(),
            source: ImageSource::Url(url.into()),
        }
    }
}

impl From<&str> for ContentPart {
    fn from(s: &str) -> Self {
        ContentPart::Text(s.to_string())
    }
}

impl From<String> for ContentPart {
    fn from(s: String) -> Self {
        ContentPart::Text(s)
    }
}

/// Image source
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ImageSource {
    /// URL to the image (not all models support this)
    Url(String),

    /// Base64 encoded image data
    Base64(Arc<str>),
}

/// Tool call from the assistant
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCall {
    pub call_id: String,
    pub fn_name: String,
    pub fn_arguments: Value,
}

/// Tool response
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolResponse {
    pub call_id: String,
    pub content: String,
    /// Whether this tool response represents an error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ToolResponse {
    /// Create a new tool response
    pub fn new(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: None,
        }
    }
}

/// Content blocks for providers that need exact sequence preservation (e.g. Anthropic with thinking)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ContentBlock {
    /// Text content
    Text {
        text: String,
        /// Optional thought signature for Gemini-style thinking
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Thinking content (Anthropic)
    Thinking {
        text: String,
        /// Signature for maintaining context across turns
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Redacted thinking content (Anthropic) - encrypted/hidden thinking
    RedactedThinking { data: String },
    /// Tool use request
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Optional thought signature for Gemini-style thinking
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Tool result response
    ToolResult {
        tool_use_id: String,
        content: String,
        /// Whether this tool result represents an error
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        /// Optional thought signature for Gemini-style thinking
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

/// A response generated by an agent (simplified for export/import)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub content: Vec<MessageContent>,
    pub reasoning: Option<String>,
    pub metadata: ResponseMetadata,
}

/// Metadata for a response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time: Option<chrono::Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<serde_json::Value>, // Simplified from genai::Usage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub custom: serde_json::Value,
}

impl Default for ResponseMetadata {
    fn default() -> Self {
        Self {
            processing_time: None,
            tokens_used: None,
            model_used: None,
            confidence: None,
            custom: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Type of relationship between an agent and a message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRelationType {
    /// Message is in the agent's active context window
    Active,
    /// Message has been compressed/archived to save context
    Archived,
    /// Message is shared from another agent/conversation
    Shared,
}

impl std::fmt::Display for MessageRelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Archived => write!(f, "archived"),
            Self::Shared => write!(f, "shared"),
        }
    }
}

/// Edge entity for agent-message relationships
///
/// This allows messages to be shared between agents and tracks
/// the relationship type and ordering.
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "agent_messages", edge = true)]
pub struct AgentMessageRelation {
    /// Edge entity ID (generated by SurrealDB)
    pub id: RelationId,

    /// The agent in this relationship
    pub in_id: AgentId,

    /// The message in this relationship
    pub out_id: MessageId,

    /// Type of relationship
    pub message_type: MessageRelationType,

    /// Position in the agent's message history (for ordering)
    /// Stores a Snowflake ID as a string for distributed monotonic ordering
    pub position: Option<SnowflakePosition>,

    /// When this relationship was created
    pub added_at: DateTime<Utc>,

    // Batch tracking fields (duplicated from Message for query efficiency)
    /// ID of the batch this message belongs to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<SnowflakePosition>,

    /// Position within the batch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_num: Option<u32>,

    /// Type of processing cycle
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_type: Option<BatchType>,
}

impl Default for AgentMessageRelation {
    fn default() -> Self {
        Self {
            id: RelationId::nil(),
            in_id: AgentId::generate(),
            out_id: MessageId::generate(),
            message_type: MessageRelationType::Active,
            position: None,
            added_at: Utc::now(),
            batch: None,
            sequence_num: None,
            batch_type: None,
        }
    }
}

// Message constructors for tests and export/import
impl Message {
    /// Check if content contains tool calls
    fn content_has_tool_calls(content: &MessageContent) -> bool {
        match content {
            MessageContent::ToolCalls(_) => true,
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. })),
            _ => false,
        }
    }

    /// Estimate word count for content
    fn estimate_word_count(content: &MessageContent) -> u32 {
        match content {
            MessageContent::Text(text) => text.split_whitespace().count() as u32,
            MessageContent::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text(text) => text.split_whitespace().count() as u32,
                    _ => 100,
                })
                .sum(),
            MessageContent::ToolCalls(calls) => calls.len() as u32 * 500,
            MessageContent::ToolResponses(responses) => responses
                .iter()
                .map(|r| r.content.split_whitespace().count() as u32)
                .sum(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text, .. } => text.split_whitespace().count() as u32,
                    ContentBlock::Thinking { text, .. } => text.split_whitespace().count() as u32,
                    ContentBlock::RedactedThinking { .. } => 1000,
                    ContentBlock::ToolUse { .. } => 500,
                    ContentBlock::ToolResult { content, .. } => {
                        content.split_whitespace().count() as u32
                    }
                })
                .sum(),
        }
    }

    /// Create a user message with the given content
    pub fn user(content: impl Into<MessageContent>) -> Self {
        let content = content.into();
        let has_tool_calls = Self::content_has_tool_calls(&content);
        let word_count = Self::estimate_word_count(&content);

        Self {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content,
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls,
            word_count,
            created_at: Utc::now(),
            position: None,
            batch: None,
            sequence_num: None,
            batch_type: Some(BatchType::UserRequest),
            embedding: None,
            embedding_model: None,
        }
    }

    /// Create a system message with the given content
    pub fn system(content: impl Into<MessageContent>) -> Self {
        let content = content.into();
        let has_tool_calls = Self::content_has_tool_calls(&content);
        let word_count = Self::estimate_word_count(&content);
        let position = get_next_message_position_sync();

        Self {
            id: MessageId::generate(),
            role: ChatRole::System,
            owner_id: None,
            content,
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls,
            word_count,
            created_at: Utc::now(),
            position: Some(position),
            batch: Some(position),
            sequence_num: Some(0),
            batch_type: Some(BatchType::SystemTrigger),
            embedding: None,
            embedding_model: None,
        }
    }

    /// Create an agent (assistant) message with the given content
    pub fn agent(content: impl Into<MessageContent>) -> Self {
        let content = content.into();
        let has_tool_calls = Self::content_has_tool_calls(&content);
        let word_count = Self::estimate_word_count(&content);
        let position = get_next_message_position_sync();

        Self {
            id: MessageId::generate(),
            role: ChatRole::Assistant,
            owner_id: None,
            content,
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls,
            word_count,
            created_at: Utc::now(),
            position: Some(position),
            batch: None,
            sequence_num: None,
            batch_type: None,
            embedding: None,
            embedding_model: None,
        }
    }

    /// Create a tool response message
    pub fn tool(responses: Vec<ToolResponse>) -> Self {
        let content = MessageContent::ToolResponses(responses);
        let word_count = Self::estimate_word_count(&content);
        let position = get_next_message_position_sync();

        Self {
            id: MessageId::generate(),
            role: ChatRole::Tool,
            owner_id: None,
            content,
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count,
            created_at: Utc::now(),
            position: Some(position),
            batch: None,
            sequence_num: None,
            batch_type: None,
            embedding: None,
            embedding_model: None,
        }
    }

    /// Create a user message in a specific batch
    pub fn user_in_batch(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        content: impl Into<MessageContent>,
    ) -> Self {
        let mut msg = Self::user(content);
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg.batch_type = Some(BatchType::UserRequest);
        msg
    }

    /// Create an assistant message in a specific batch
    pub fn assistant_in_batch(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        content: impl Into<MessageContent>,
    ) -> Self {
        let mut msg = Self::agent(content);
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg
    }

    /// Create a tool response message in a specific batch
    pub fn tool_in_batch(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        responses: Vec<ToolResponse>,
    ) -> Self {
        let mut msg = Self::tool(responses);
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg
    }
}
