//! Content block iteration for processing responses.
//!
//! Provides a unified view over different MessageContent formats without
//! transforming the underlying storage. This handles both:
//! - `MessageContent::ToolCalls(vec)` - Direct tool call list
//! - `MessageContent::Blocks` with `ContentBlock::ToolUse` - Anthropic's native format

use crate::messages::{ContentBlock, MessageContent};

/// Unified view for iteration over response content.
///
/// This doesn't transform the underlying data, just provides a common
/// iteration interface over the different content formats.
#[derive(Debug, Clone)]
pub enum ContentItem<'a> {
    /// Text content from the model
    Text(&'a str),
    /// Thinking/reasoning content (Anthropic extended thinking)
    Thinking(&'a str),
    /// Tool use request
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Value,
    },
    /// Other content types we don't need to process inline
    Other,
}

/// Iterate over content items in a response.
///
/// Handles both `MessageContent::ToolCalls` and `MessageContent::Blocks` formats,
/// yielding a unified `ContentItem` for each piece of content.
///
/// # Example
/// ```ignore
/// for item in iter_content_items(&response.content) {
///     match item {
///         ContentItem::Text(text) => { /* emit event */ }
///         ContentItem::Thinking(text) => { /* emit reasoning event */ }
///         ContentItem::ToolUse { id, name, input } => { /* execute tool */ }
///         ContentItem::Other => {}
///     }
/// }
/// ```
pub fn iter_content_items(content: &[MessageContent]) -> impl Iterator<Item = ContentItem<'_>> {
    content.iter().flat_map(|mc| match mc {
        MessageContent::Text(text) => vec![ContentItem::Text(text)],

        MessageContent::ToolCalls(calls) => calls
            .iter()
            .map(|c| ContentItem::ToolUse {
                id: &c.call_id,
                name: &c.fn_name,
                input: &c.fn_arguments,
            })
            .collect(),

        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text, .. } => ContentItem::Text(text),
                ContentBlock::Thinking { text, .. } => ContentItem::Thinking(text),
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => ContentItem::ToolUse { id, name, input },
                _ => ContentItem::Other,
            })
            .collect(),

        MessageContent::Parts(_) => vec![ContentItem::Other],
        MessageContent::ToolResponses(_) => vec![ContentItem::Other],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::ToolCall;
    use serde_json::json;

    #[test]
    fn test_iter_text_content() {
        let content = vec![MessageContent::Text("Hello world".to_string())];
        let items: Vec<_> = iter_content_items(&content).collect();

        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], ContentItem::Text("Hello world")));
    }

    #[test]
    fn test_iter_tool_calls() {
        let content = vec![MessageContent::ToolCalls(vec![
            ToolCall {
                call_id: "call_1".to_string(),
                fn_name: "test_tool".to_string(),
                fn_arguments: json!({"arg": "value"}),
            },
            ToolCall {
                call_id: "call_2".to_string(),
                fn_name: "other_tool".to_string(),
                fn_arguments: json!({}),
            },
        ])];

        let items: Vec<_> = iter_content_items(&content).collect();

        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0],
            ContentItem::ToolUse {
                id: "call_1",
                name: "test_tool",
                ..
            }
        ));
        assert!(matches!(
            items[1],
            ContentItem::ToolUse {
                id: "call_2",
                name: "other_tool",
                ..
            }
        ));
    }

    #[test]
    fn test_iter_blocks_mixed() {
        let content = vec![MessageContent::Blocks(vec![
            ContentBlock::Thinking {
                text: "Let me think...".to_string(),
                signature: None,
            },
            ContentBlock::Text {
                text: "Here's my answer".to_string(),
                thought_signature: None,
            },
            ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "search".to_string(),
                input: json!({"query": "test"}),
                thought_signature: None,
            },
        ])];

        let items: Vec<_> = iter_content_items(&content).collect();

        assert_eq!(items.len(), 3);
        assert!(matches!(items[0], ContentItem::Thinking("Let me think...")));
        assert!(matches!(items[1], ContentItem::Text("Here's my answer")));
        assert!(matches!(
            items[2],
            ContentItem::ToolUse { name: "search", .. }
        ));
    }

    #[test]
    fn test_iter_multiple_content_types() {
        let content = vec![
            MessageContent::Text("First message".to_string()),
            MessageContent::ToolCalls(vec![ToolCall {
                call_id: "call_1".to_string(),
                fn_name: "tool".to_string(),
                fn_arguments: json!({}),
            }]),
        ];

        let items: Vec<_> = iter_content_items(&content).collect();

        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], ContentItem::Text("First message")));
        assert!(matches!(
            items[1],
            ContentItem::ToolUse { name: "tool", .. }
        ));
    }
}
