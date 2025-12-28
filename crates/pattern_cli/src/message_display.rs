//! Helper functions for displaying messages in a consistent format

use owo_colors::OwoColorize;
use pattern_core::messages::{ChatRole, ContentBlock, ContentPart, Message, MessageContent};

use crate::output::Output;

/// Display a message with proper formatting and role coloring
pub fn display_message(
    msg: &Message,
    output: &Output,
    show_id: bool,
    preview_length: Option<usize>,
) {
    // Format role display
    let role_str = match msg.role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    };

    let role_display = match msg.role {
        ChatRole::System => role_str.bright_blue().to_string(),
        ChatRole::User => role_str.bright_green().to_string(),
        ChatRole::Assistant => role_str.bright_cyan().to_string(),
        ChatRole::Tool => role_str.bright_yellow().to_string(),
    };

    if show_id {
        output.kv(
            "Role",
            &format!("{} ({})", role_display, msg.id.0.to_string().dimmed()),
        );
    } else {
        output.kv("Role", &role_display);
    }

    // Show timestamp
    output.kv(
        "Time",
        &msg.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    );

    // Show batch/position/sequence info
    let batch_info = format!(
        "batch: {}, pos: {}, seq: {}",
        msg.batch
            .as_ref()
            .map(|b| b.to_string())
            .unwrap_or_else(|| "none".to_string()),
        msg.position
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string()),
        msg.sequence_num
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    output.kv("Batch Info", &batch_info.dimmed().to_string());

    // Display content based on type
    let text = msg.display_content();
    if !text.is_empty() {
        let display_text = if let Some(max_len) = preview_length {
            if text.len() > max_len {
                format!("{}...", text.chars().take(max_len).collect::<String>())
            } else {
                text.clone()
            }
        } else {
            text.clone()
        };

        output.kv("Content", &display_text.dimmed().to_string());
    } else {
        // Handle non-text content with detailed breakdown
        display_message_content(&msg.content, output, preview_length);
    }
}

/// Display detailed message content for non-text messages
pub fn display_message_content(
    content: &MessageContent,
    output: &Output,
    preview_length: Option<usize>,
) {
    let preview_len = preview_length.unwrap_or(100);

    match content {
        MessageContent::Parts(parts) => {
            output.kv("Content", &format!("[Multi-part: {} parts]", parts.len()));
            for (j, part) in parts.iter().enumerate().take(3) {
                match part {
                    ContentPart::Text(t) => {
                        let preview = if t.len() > preview_len {
                            format!("{}...", t.chars().take(preview_len).collect::<String>())
                        } else {
                            t.to_string()
                        };
                        output.list_item(&format!("Part {}: Text - {}", j + 1, preview.dimmed()));
                    }
                    ContentPart::Image { content_type, .. } => {
                        output.list_item(&format!("Part {}: [Image: {}]", j + 1, content_type));
                    }
                }
            }
            if parts.len() > 3 {
                output.list_item(&format!("... and {} more parts", parts.len() - 3));
            }
        }
        MessageContent::ToolCalls(calls) => {
            output.kv("Content", &format!("[Tool calls: {} calls]", calls.len()));
            for (j, call) in calls.iter().enumerate().take(3) {
                output.list_item(&format!(
                    "Call {}: {} (id: {})",
                    j + 1,
                    call.fn_name,
                    call.call_id
                ));
            }
            if calls.len() > 3 {
                output.list_item(&format!("... and {} more calls", calls.len() - 3));
            }
        }
        MessageContent::ToolResponses(responses) => {
            output.kv(
                "Content",
                &format!("[Tool responses: {} responses]", responses.len()),
            );
            for (j, resp) in responses.iter().enumerate().take(3) {
                let content_preview = if resp.content.len() > preview_len {
                    format!(
                        "{}...",
                        resp.content.chars().take(preview_len).collect::<String>()
                    )
                } else {
                    resp.content.clone()
                };
                output.list_item(&format!(
                    "Response {} (call_id: {}): {}",
                    j + 1,
                    resp.call_id,
                    content_preview.dimmed()
                ));
            }
            if responses.len() > 3 {
                output.list_item(&format!("... and {} more responses", responses.len() - 3));
            }
        }
        MessageContent::Blocks(blocks) => {
            output.kv("Content", &format!("[Blocks: {} blocks]", blocks.len()));
            for (j, block) in blocks.iter().enumerate().take(3) {
                match block {
                    ContentBlock::Text { text, .. } => {
                        let preview = if text.len() > preview_len {
                            format!("{}...", text.chars().take(preview_len).collect::<String>())
                        } else {
                            text.clone()
                        };
                        output.list_item(&format!("Block {}: Text - {}", j + 1, preview.dimmed()));
                    }
                    ContentBlock::Thinking { text, .. } => {
                        let preview = if text.len() > preview_len {
                            format!("{}...", text.chars().take(preview_len).collect::<String>())
                        } else {
                            text.clone()
                        };
                        output.list_item(&format!(
                            "Block {}: Thinking - {}",
                            j + 1,
                            preview.dimmed()
                        ));
                    }
                    ContentBlock::RedactedThinking { .. } => {
                        output.list_item(&format!("Block {}: [Redacted Thinking]", j + 1));
                    }
                    ContentBlock::ToolUse { name, id, .. } => {
                        output.list_item(&format!(
                            "Block {}: Tool Use - {} (id: {})",
                            j + 1,
                            name,
                            id
                        ));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let preview = if content.len() > preview_len {
                            format!(
                                "{}...",
                                content.chars().take(preview_len).collect::<String>()
                            )
                        } else {
                            content.clone()
                        };
                        output.list_item(&format!(
                            "Block {}: Tool Result (tool_use_id: {}) - {}",
                            j + 1,
                            tool_use_id,
                            preview.dimmed()
                        ));
                    }
                }
            }
            if blocks.len() > 3 {
                output.list_item(&format!("... and {} more blocks", blocks.len() - 3));
            }
        }
        _ => {
            output.kv("Content", "[Non-text content]");
        }
    }
}

/// Display a simple one-line message summary
pub fn display_message_summary(msg: &Message, max_length: usize) -> String {
    let role_str = match msg.role {
        ChatRole::User => "user".bright_green().to_string(),
        ChatRole::Assistant => "assistant".bright_blue().to_string(),
        ChatRole::System => "system".bright_yellow().to_string(),
        ChatRole::Tool => "tool".bright_magenta().to_string(),
    };

    let text = msg.display_content();
    let text = if text.is_empty() {
        "(no text content)".to_string()
    } else {
        text
    };
    let preview = if text.len() > max_length {
        format!("{}...", text.chars().take(max_length).collect::<String>())
    } else {
        text
    };

    format!("{}: {}", role_str, preview)
}
