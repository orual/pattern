use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::memory::CONSTELLATION_OWNER;

/// Reference to a memory block for loading into context
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
pub struct BlockRef {
    /// Human-readable label for context display
    pub label: String,
    /// Database block ID
    pub block_id: String,
    /// Owner agent ID, defaults to "_constellation_" for shared blocks
    pub agent_id: String,
}

impl BlockRef {
    /// Create a new block ref with constellation as default owner
    pub fn new(label: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: CONSTELLATION_OWNER.to_string(),
        }
    }

    /// Create a block ref with explicit owner
    pub fn with_owner(
        label: impl Into<String>,
        block_id: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: agent_id.into(),
        }
    }

    /// Set the owner agent ID (builder pattern)
    pub fn owned_by(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }
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
    /// Block references to load for this message's context
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_refs: Vec<BlockRef>,
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
