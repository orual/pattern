//! Embedding-related rendering helpers.
//!
//! Functions in this module convert runtime objects (messages, etc.) into
//! the canonical text shape used for embedding generation. Shared between
//! the forward write paths (agent_loop, etc.) and the offline backfill
//! command in pattern_cli.

use pattern_core::types::message::Message;

/// Render a message into the text used for embedding generation.
///
/// Concatenates semantic signal from all content parts:
/// - all text parts (not just the first)
/// - thinking blocks (model reasoning text, prefixed `[thinking]`)
/// - tool call function names (prefixed `[tool: NAME]`)
/// - tool response text payloads when present (prefixed `[tool result]`)
///
/// Binary parts (images, PDFs), tool-call argument JSON, and Custom parts
/// are intentionally skipped — they don't carry text signal that helps
/// semantic retrieval, and indexing them dilutes the embedding vector
/// with structural noise.
pub fn render_message_for_embedding(msg: &Message) -> String {
    render_chat_message_for_embedding(&msg.chat_message)
}

/// Variant taking a [`genai::chat::ChatMessage`] directly.
///
/// Useful when callers already have the chat message extracted (e.g.
/// after deserializing `content_json` from the messages table).
pub fn render_chat_message_for_embedding(msg: &genai::chat::ChatMessage) -> String {
    use genai::chat::ContentPart;

    let mut parts: Vec<String> = Vec::new();
    for part in msg.content.parts() {
        match part {
            ContentPart::Text(s) => {
                if !s.is_empty() {
                    parts.push(s.clone());
                }
            }
            ContentPart::ThinkingBlock(tb) => {
                if let Some(text) = &tb.text
                    && !text.is_empty()
                {
                    parts.push(format!("[thinking] {text}"));
                }
            }
            ContentPart::ToolCall(tc) => {
                parts.push(format!("[tool: {}]", tc.fn_name));
            }
            ContentPart::ToolResponse(tr) => {
                // Embedding text walks tool results for their joined text content.
                // Binary parts are skipped — embedding semantics are text-only.
                if let Some(s) = tr.joined_text()
                    && !s.is_empty()
                {
                    parts.push(format!("[tool result] {s}"));
                }
            }
            ContentPart::Binary(_) | ContentPart::Custom(_) => {}
        }
    }
    parts.join("\n\n")
}
