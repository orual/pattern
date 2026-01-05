//! Conversions between pattern-core message types and genai chat types

use super::*;

// From genai types to ours

impl From<genai::chat::ChatRole> for ChatRole {
    fn from(role: genai::chat::ChatRole) -> Self {
        match role {
            genai::chat::ChatRole::System => ChatRole::System,
            genai::chat::ChatRole::User => ChatRole::User,
            genai::chat::ChatRole::Assistant => ChatRole::Assistant,
            genai::chat::ChatRole::Tool => ChatRole::Tool,
        }
    }
}

impl From<genai::chat::MessageContent> for MessageContent {
    fn from(content: genai::chat::MessageContent) -> Self {
        match content {
            genai::chat::MessageContent::Text(text) => MessageContent::Text(text),
            genai::chat::MessageContent::Parts(parts) => {
                MessageContent::Parts(parts.into_iter().map(Into::into).collect())
            }
            genai::chat::MessageContent::ToolCalls(calls) => {
                MessageContent::ToolCalls(calls.into_iter().map(Into::into).collect())
            }
            genai::chat::MessageContent::ToolResponses(responses) => {
                MessageContent::ToolResponses(responses.into_iter().map(Into::into).collect())
            }
            genai::chat::MessageContent::Blocks(blocks) => {
                // Convert genai blocks to Pattern blocks
                MessageContent::Blocks(
                    blocks
                        .into_iter()
                        .map(|block| match block {
                            genai::chat::ContentBlock::Text {
                                text,
                                thought_signature,
                            } => ContentBlock::Text {
                                text,
                                thought_signature,
                            },
                            genai::chat::ContentBlock::Thinking { text, signature } => {
                                ContentBlock::Thinking { text, signature }
                            }
                            genai::chat::ContentBlock::RedactedThinking { data } => {
                                ContentBlock::RedactedThinking { data }
                            }
                            genai::chat::ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature,
                            } => ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature,
                            },
                            genai::chat::ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                thought_signature,
                            } => ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                thought_signature,
                            },
                        })
                        .collect(),
                )
            }
        }
    }
}

impl From<genai::chat::MessageOptions> for MessageOptions {
    fn from(opts: genai::chat::MessageOptions) -> Self {
        Self {
            cache_control: opts.cache_control.map(Into::into),
        }
    }
}

impl From<genai::chat::CacheControl> for CacheControl {
    fn from(cc: genai::chat::CacheControl) -> Self {
        match cc {
            genai::chat::CacheControl::Ephemeral => CacheControl::Ephemeral,
        }
    }
}

impl From<genai::chat::ContentPart> for ContentPart {
    fn from(part: genai::chat::ContentPart) -> Self {
        match part {
            genai::chat::ContentPart::Text(text) => ContentPart::Text(text),
            genai::chat::ContentPart::Image {
                content_type,
                source,
            } => ContentPart::Image {
                content_type,
                source: source.into(),
            },
        }
    }
}

impl From<genai::chat::ImageSource> for ImageSource {
    fn from(source: genai::chat::ImageSource) -> Self {
        match source {
            genai::chat::ImageSource::Url(url) => ImageSource::Url(url),
            genai::chat::ImageSource::Base64(data) => ImageSource::Base64(data),
        }
    }
}

impl From<genai::chat::ToolCall> for ToolCall {
    fn from(call: genai::chat::ToolCall) -> Self {
        Self {
            call_id: call.call_id,
            fn_name: call.fn_name,
            fn_arguments: call.fn_arguments,
        }
    }
}

impl From<genai::chat::ToolResponse> for ToolResponse {
    fn from(resp: genai::chat::ToolResponse) -> Self {
        Self {
            call_id: resp.call_id,
            content: resp.content,
            is_error: resp.is_error,
        }
    }
}

// From our types to genai

impl From<ChatRole> for genai::chat::ChatRole {
    fn from(role: ChatRole) -> Self {
        match role {
            ChatRole::System => genai::chat::ChatRole::System,
            ChatRole::User => genai::chat::ChatRole::User,
            ChatRole::Assistant => genai::chat::ChatRole::Assistant,
            ChatRole::Tool => genai::chat::ChatRole::Tool,
        }
    }
}

impl From<MessageContent> for genai::chat::MessageContent {
    fn from(content: MessageContent) -> Self {
        match content {
            MessageContent::Text(text) => genai::chat::MessageContent::Text(text),
            MessageContent::Parts(parts) => {
                genai::chat::MessageContent::Parts(parts.into_iter().map(Into::into).collect())
            }
            MessageContent::ToolCalls(calls) => {
                genai::chat::MessageContent::ToolCalls(calls.into_iter().map(Into::into).collect())
            }
            MessageContent::ToolResponses(responses) => genai::chat::MessageContent::ToolResponses(
                responses.into_iter().map(Into::into).collect(),
            ),
            MessageContent::Blocks(blocks) => {
                // Convert Pattern's blocks to genai's blocks
                genai::chat::MessageContent::Blocks(
                    blocks
                        .into_iter()
                        .map(|block| match block {
                            ContentBlock::Text {
                                text,
                                thought_signature,
                            } => genai::chat::ContentBlock::Text {
                                text,
                                thought_signature,
                            },
                            ContentBlock::Thinking { text, signature } => {
                                genai::chat::ContentBlock::Thinking { text, signature }
                            }
                            ContentBlock::RedactedThinking { data } => {
                                genai::chat::ContentBlock::RedactedThinking { data }
                            }
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature,
                            } => genai::chat::ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature,
                            },
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                thought_signature,
                            } => genai::chat::ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                thought_signature,
                            },
                        })
                        .collect(),
                )
            }
        }
    }
}

impl From<MessageOptions> for genai::chat::MessageOptions {
    fn from(opts: MessageOptions) -> Self {
        genai::chat::MessageOptions {
            cache_control: opts.cache_control.map(Into::into),
        }
    }
}

impl From<CacheControl> for genai::chat::CacheControl {
    fn from(cc: CacheControl) -> Self {
        match cc {
            CacheControl::Ephemeral => genai::chat::CacheControl::Ephemeral,
        }
    }
}

impl From<ContentPart> for genai::chat::ContentPart {
    fn from(part: ContentPart) -> Self {
        match part {
            ContentPart::Text(text) => genai::chat::ContentPart::Text(text),
            ContentPart::Image {
                content_type,
                source,
            } => genai::chat::ContentPart::Image {
                content_type,
                source: source.into(),
            },
        }
    }
}

impl From<ImageSource> for genai::chat::ImageSource {
    fn from(source: ImageSource) -> Self {
        match source {
            ImageSource::Url(url) => genai::chat::ImageSource::Url(url),
            ImageSource::Base64(data) => genai::chat::ImageSource::Base64(data),
        }
    }
}

impl From<ToolCall> for genai::chat::ToolCall {
    fn from(call: ToolCall) -> Self {
        genai::chat::ToolCall {
            call_id: call.call_id,
            fn_name: call.fn_name,
            fn_arguments: call.fn_arguments,
        }
    }
}

impl From<ToolResponse> for genai::chat::ToolResponse {
    fn from(resp: ToolResponse) -> Self {
        genai::chat::ToolResponse {
            call_id: resp.call_id,
            content: resp.content,
            is_error: resp.is_error,
        }
    }
}
