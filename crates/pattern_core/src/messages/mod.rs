//! Message storage and coordination.
//!
//! This module provides the MessageStore wrapper for agent-scoped message operations,
//! along with re-exports of relevant types.

pub mod batch;
pub mod conversions;
pub mod queue;
pub mod response;
mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use batch::*;
pub use response::*;
pub use store::MessageStore;
pub use types::*;
// Re-export other message types from pattern_db
pub use pattern_db::models::{ArchiveSummary, MessageSummary};

// Re-export coordination types from pattern_db
pub use pattern_db::models::{
    ActivityEvent, ActivityEventType, AgentSummary, ConstellationSummary, CoordinationState,
    CoordinationTask, EventImportance, HandoffNote, NotableEvent, TaskPriority, TaskStatus,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{MessageId, UserId};
use crate::{SnowflakePosition, utils::get_next_message_position_sync};

/// A message to be processed by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for Message {
    fn default() -> Self {
        let position = get_next_message_position_sync();
        Self {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::Text(String::new()),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 0,
            created_at: Utc::now(),
            position: Some(position),
            batch: Some(position), // First message in its own batch
            sequence_num: Some(0),
            batch_type: Some(BatchType::UserRequest),
        }
    }
}

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
            MessageContent::ToolCalls(calls) => calls.len() as u32 * 500, // Estimate
            MessageContent::ToolResponses(responses) => responses
                .iter()
                .map(|r| r.content.split_whitespace().count() as u32)
                .sum(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text, .. } => text.split_whitespace().count() as u32,
                    ContentBlock::Thinking { text, .. } => text.split_whitespace().count() as u32,
                    ContentBlock::RedactedThinking { .. } => 1000, // Estimate
                    ContentBlock::ToolUse { .. } => 500,           // Estimate
                    ContentBlock::ToolResult { content, .. } => {
                        content.split_whitespace().count() as u32
                    }
                })
                .sum(),
        }
    }

    /// Convert this message to a genai ChatMessage
    pub fn as_chat_message(&self) -> genai::chat::ChatMessage {
        // Handle Gemini's requirement that ToolResponses must have Tool role
        // If we have ToolResponses with a non-Tool role, fix it
        let role = match (&self.role, &self.content) {
            (role, MessageContent::ToolResponses(_)) if !role.is_tool() => {
                tracing::warn!(
                    "Found ToolResponses with incorrect role {:?}, converting to Tool role",
                    role
                );
                ChatRole::Tool
            }
            _ => self.role.clone(),
        };

        // Debug log to track what content types are being sent
        let content = match &self.content {
            MessageContent::Text(text) => {
                tracing::trace!("Converting Text message with role {:?}", role);
                MessageContent::Text(text.trim().to_string())
            }
            MessageContent::ToolCalls(_) => {
                tracing::trace!("Converting ToolCalls message with role {:?}", role);
                self.content.clone()
            }
            MessageContent::ToolResponses(_) => {
                tracing::trace!("Converting ToolResponses message with role {:?}", role);
                self.content.clone()
            }
            MessageContent::Parts(parts) => match role {
                ChatRole::System | ChatRole::Assistant | ChatRole::Tool => {
                    tracing::trace!("Combining Parts message with role {:?}", role);
                    let string = parts
                        .into_iter()
                        .map(|part| match part {
                            ContentPart::Text(text) => text.trim().to_string(),
                            ContentPart::Image {
                                content_type,
                                source,
                            } => {
                                let source_as_text = match source {
                                    ImageSource::Url(st) => st.trim().to_string(),
                                    ImageSource::Base64(st) => st.trim().to_string(),
                                };
                                format!("{}: {}", content_type, source_as_text)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n---\n");
                    MessageContent::Text(string)
                }
                ChatRole::User => self.content.clone(),
            },
            MessageContent::Blocks(_) => self.content.clone(),
        };

        genai::chat::ChatMessage {
            role: role.into(),
            content: content.into(),
            options: Some(self.options.clone().into()),
        }
    }
}

impl Message {
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
            // Standalone user messages do not belong to a batch yet.
            // Batches are assigned by higher-level flows when appropriate.
            position: None,
            batch: None,
            sequence_num: None,
            batch_type: Some(BatchType::UserRequest),
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
            batch: Some(position), // System messages start new batches
            sequence_num: Some(0),
            batch_type: Some(BatchType::SystemTrigger),
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
            batch: None,        // Will be set by batch-aware constructor
            sequence_num: None, // Will be set by batch-aware constructor
            batch_type: None,   // Will be set by batch-aware constructor
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
            batch: None,        // Will be set by batch-aware constructor
            sequence_num: None, // Will be set by batch-aware constructor
            batch_type: None,   // Will be set by batch-aware constructor
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
        // Batch type could be anything, caller should set if not UserRequest
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
        // Batch type inherited from batch context
        msg
    }

    /// Create a system message in a specific batch
    pub fn system_in_batch(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        content: impl Into<MessageContent>,
    ) -> Self {
        let mut msg = Self::system(content);
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg.batch_type = Some(BatchType::Continuation);
        msg
    }

    /// Create a user message in a specific batch with explicit batch type
    pub fn user_in_batch_typed(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        batch_type: BatchType,
        content: impl Into<MessageContent>,
    ) -> Self {
        let mut msg = Self::user(content);
        msg.position = Some(crate::utils::get_next_message_position_sync());
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg.batch_type = Some(batch_type);
        msg
    }

    /// Create a tool response message in a specific batch with explicit batch type
    pub fn tool_in_batch_typed(
        batch_id: SnowflakePosition,
        sequence_num: u32,
        batch_type: BatchType,
        responses: Vec<ToolResponse>,
    ) -> Self {
        let mut msg = Self::tool(responses);
        msg.position = Some(crate::utils::get_next_message_position_sync());
        msg.batch = Some(batch_id);
        msg.sequence_num = Some(sequence_num);
        msg.batch_type = Some(batch_type);
        msg
    }

    /// Create Messages from an agent Response
    pub fn from_response(
        response: &Response,
        agent_id: &crate::AgentId,
        batch_id: Option<SnowflakePosition>,
        batch_type: Option<BatchType>,
    ) -> Vec<Self> {
        let mut messages = Vec::new();

        // Group assistant content together, but keep tool responses separate
        let mut current_assistant_content: Vec<MessageContent> = Vec::new();

        for content in &response.content {
            match content {
                MessageContent::ToolResponses(_) => {
                    // First, flush any accumulated assistant content
                    if !current_assistant_content.is_empty() {
                        let combined_content = if current_assistant_content.len() == 1 {
                            current_assistant_content[0].clone()
                        } else {
                            // Combine multiple content items - for now just take the first
                            // TODO: properly combine Text + ToolCalls
                            current_assistant_content[0].clone()
                        };

                        let has_tool_calls =
                            matches!(&combined_content, MessageContent::ToolCalls(_));
                        let word_count = Self::estimate_word_count(&combined_content);

                        let position = crate::utils::get_next_message_position_sync();

                        messages.push(Self {
                            id: MessageId::generate(),
                            role: ChatRole::Assistant,
                            content: combined_content,
                            metadata: MessageMetadata {
                                user_id: Some(agent_id.to_record_id()),
                                ..Default::default()
                            },
                            options: MessageOptions::default(),
                            created_at: Utc::now(),
                            owner_id: None,
                            has_tool_calls,
                            word_count,
                            position: Some(position),
                            batch: batch_id,
                            sequence_num: None, // Will be set by batch
                            batch_type,
                        });
                        current_assistant_content.clear();
                    }

                    // Then add the tool response as a separate message
                    let position = crate::utils::get_next_message_position_sync();

                    messages.push(Self {
                        id: MessageId::generate(),
                        role: ChatRole::Tool,
                        content: content.clone(),
                        metadata: MessageMetadata {
                            user_id: Some(agent_id.to_record_id()),
                            ..Default::default()
                        },
                        options: MessageOptions::default(),
                        created_at: Utc::now(),
                        owner_id: None,
                        has_tool_calls: false,
                        word_count: Self::estimate_word_count(content),
                        position: Some(position),
                        batch: batch_id,
                        sequence_num: None, // Will be set by batch
                        batch_type,
                    });
                }
                _ => {
                    // Accumulate assistant content
                    current_assistant_content.push(content.clone());
                }
            }
        }

        // Flush any remaining assistant content
        if !current_assistant_content.is_empty() {
            let combined_content = if current_assistant_content.len() == 1 {
                current_assistant_content[0].clone()
            } else {
                // TODO: properly combine multiple content items
                current_assistant_content[0].clone()
            };

            let has_tool_calls = Self::content_has_tool_calls(&combined_content);
            let word_count = Self::estimate_word_count(&combined_content);

            let position = crate::utils::get_next_message_position_sync();

            messages.push(Self {
                id: MessageId::generate(),
                role: ChatRole::Assistant,
                content: combined_content,
                metadata: MessageMetadata {
                    user_id: Some(agent_id.to_string()),
                    ..Default::default()
                },
                options: MessageOptions::default(),
                created_at: Utc::now(),
                owner_id: None,
                has_tool_calls,
                word_count,
                position: Some(position),
                batch: batch_id,
                sequence_num: None, // Will be set by batch
                batch_type,
            });
        }

        messages
    }

    /// Extract text content from the message if available
    ///
    /// Returns None if the message contains only non-text content (e.g., tool calls)
    pub fn text_content(&self) -> Option<String> {
        match &self.content {
            MessageContent::Text(text) => Some(text.clone()),
            MessageContent::Parts(parts) => {
                // Concatenate all text parts
                let text_parts: Vec<String> = parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect();

                if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join(" "))
                }
            }
            _ => None,
        }
    }

    /// Extract displayable content from the message for search/display purposes
    ///
    /// Unlike text_content(), this extracts text from tool calls, reasoning blocks,
    /// and other structured content that should be searchable
    pub fn display_content(&self) -> String {
        match &self.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => {
                // Concatenate all text parts
                parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text(text) => Some(text.clone()),
                        ContentPart::Image {
                            content_type,
                            source,
                        } => {
                            // Include image description for searchability
                            let source_info = match source {
                                ImageSource::Url(url) => format!("[Image URL: {}]", url),
                                ImageSource::Base64(_) => "[Base64 Image]".to_string(),
                            };
                            Some(format!("[Image: {}] {}", content_type, source_info))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            MessageContent::ToolCalls(calls) => {
                // Just dump the JSON for tool calls
                calls
                    .iter()
                    .map(|call| {
                        format!(
                            "[Tool: {}] {}",
                            call.fn_name,
                            serde_json::to_string_pretty(&call.fn_arguments)
                                .unwrap_or_else(|_| "{}".to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            MessageContent::ToolResponses(responses) => {
                // Include tool response content
                responses
                    .iter()
                    .map(|resp| format!("[Tool Response] {}", resp.content))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            MessageContent::Blocks(blocks) => {
                // Extract text from all block types including reasoning
                blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        ContentBlock::Thinking { text, .. } => {
                            // Include reasoning content for searchability
                            Some(format!("[Reasoning] {}", text))
                        }
                        ContentBlock::RedactedThinking { .. } => {
                            // Note redacted thinking but don't include content
                            Some("[Redacted Reasoning]".to_string())
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            // Just dump the JSON
                            Some(format!(
                                "[Tool: {}] {}",
                                name,
                                serde_json::to_string_pretty(input)
                                    .unwrap_or_else(|_| "{}".to_string())
                            ))
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            Some(format!("[Tool Result] {}", content))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// Check if this message contains tool calls
    pub fn has_tool_calls(&self) -> bool {
        match &self.content {
            MessageContent::ToolCalls(_) => true,
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. })),
            _ => false,
        }
    }

    /// Get the number of tool calls in this message
    pub fn tool_call_count(&self) -> usize {
        match &self.content {
            MessageContent::ToolCalls(calls) => calls.len(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
                .count(),
            _ => 0,
        }
    }

    /// Get the number of tool responses in this message
    pub fn tool_response_count(&self) -> usize {
        match &self.content {
            MessageContent::ToolResponses(calls) => calls.len(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
                .count(),
            _ => 0,
        }
    }

    /// Rough estimation of token count for this message
    ///
    /// Uses the approximation of ~4 characters per token
    /// Images are estimated at 1200 tokens each
    pub fn estimate_tokens(&self) -> usize {
        let text_tokens = self.display_content().len() / 5;

        // Count images in the message
        let image_count = match &self.content {
            MessageContent::Parts(parts) => parts
                .iter()
                .filter(|part| matches!(part, ContentPart::Image { .. }))
                .count(),
            _ => 0,
        };

        text_tokens + (image_count * 1200)
    }
}

/// Parse text content for multimodal markers and convert to ContentParts
///
/// Looks for [IMAGE: url] markers in text and converts them to proper ContentPart::Image entries.
/// Takes only the last 4 images to avoid token bloat.
pub fn parse_multimodal_markers(text: &str) -> Option<Vec<ContentPart>> {
    // Regex to find [IMAGE: url] markers
    let image_pattern = regex::Regex::new(r"\[IMAGE:\s*([^\]]+)\]").ok()?;

    let mut parts = Vec::new();
    let mut last_end = 0;
    let mut image_markers = Vec::new();

    // Collect all image markers with their positions
    for cap in image_pattern.captures_iter(text) {
        let full_match = cap.get(0)?;
        let url = cap.get(1)?.as_str().trim();

        image_markers.push((full_match.start(), full_match.end(), url.to_string()));
    }

    // If no images found, return None to keep original text format
    if image_markers.is_empty() {
        return None;
    }

    // Take only the last 4 images
    let selected_images: Vec<_> = image_markers.iter().rev().take(4).rev().cloned().collect();

    // Build parts, including only selected images
    for (start, end, url) in &image_markers {
        // Add text before this marker
        if *start > last_end {
            let text_part = text[last_end..*start].trim();
            if !text_part.is_empty() {
                parts.push(ContentPart::Text(text_part.to_string()));
            }
        }

        // Only add image if it's in our selected set
        if selected_images.iter().any(|(_, _, u)| u == url) {
            // Debug log the URL being processed
            tracing::debug!("Processing image URL: {}", url);

            // Determine if this is base64 or URL
            let source = if url.starts_with("data:") || url.starts_with("base64:") {
                // Extract base64 data
                let data = if let Some(comma_pos) = url.find(',') {
                    &url[comma_pos + 1..]
                } else {
                    url
                };
                tracing::debug!("Creating Base64 ImageSource from URL: {}", url);
                ImageSource::Base64(Arc::from(data))
            } else {
                tracing::debug!("Creating URL ImageSource from URL: {}", url);
                ImageSource::Url(url.clone())
            };

            // Try to infer content type
            let content_type = if url.contains(".png") || url.contains("image/png") {
                "image/png"
            } else if url.contains(".gif") || url.contains("image/gif") {
                "image/gif"
            } else if url.contains(".webp") || url.contains("image/webp") {
                "image/webp"
            } else {
                "image/jpeg" // Default to JPEG
            }
            .to_string();

            parts.push(ContentPart::Image {
                content_type,
                source,
            });
        }

        last_end = *end;
    }

    // Add any remaining text after the last marker
    if last_end < text.len() {
        let text_part = text[last_end..].trim();
        if !text_part.is_empty() {
            parts.push(ContentPart::Text(text_part.to_string()));
        }
    }

    // Only return Parts if we actually added images
    let has_images = parts.iter().any(|p| matches!(p, ContentPart::Image { .. }));
    if has_images { Some(parts) } else { None }
}
