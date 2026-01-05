//! Search utilities for scoring adjustments and snippet extraction

use crate::messages::{ContentBlock, Message, MessageContent};
use serde::{Deserialize, Serialize};

/// Search result with relevance score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMessage {
    pub message: Message,
    pub score: f32,
}

/// Search result for constellation messages with relevance score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredConstellationMessage {
    pub agent_name: String,
    pub message: Message,
    pub score: f32,
}

/// Adjust score based on content type (downrank reasoning/tool responses)
pub fn adjust_message_score(msg: &Message, base_score: f32) -> f32 {
    let mut score = base_score;

    // Check if content is primarily reasoning or tool responses
    match &msg.content {
        MessageContent::Blocks(blocks) => {
            let total_blocks = blocks.len();
            let reasoning_blocks = blocks
                .iter()
                .filter(|b| {
                    matches!(
                        b,
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                    )
                })
                .count();
            let tool_blocks = blocks
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                .count();

            // Downrank if mostly reasoning/tools
            let non_content_ratio =
                (reasoning_blocks + tool_blocks) as f32 / total_blocks.max(1) as f32;
            score *= 1.0 - (non_content_ratio * 0.5); // Reduce score by up to 50%
        }
        MessageContent::ToolResponses(_) => {
            score *= 0.7; // Tool responses get 30% penalty
        }
        _ => {} // Regular text content keeps full score
    }

    score
}

/// Extract a snippet around the search query
pub fn extract_snippet(content: &str, query: &str, max_length: usize) -> String {
    let lower_content = content.to_lowercase();
    let lower_query = query.to_lowercase();

    if let Some(pos) = lower_content.find(&lower_query) {
        // Calculate context window around match
        let context_before = 50;
        let context_after = max_length.saturating_sub(context_before + query.len());

        let mut start = pos.saturating_sub(context_before);
        let mut end = (pos + query.len() + context_after).min(content.len());

        // Ensure we're at char boundaries
        while start > 0 && !content.is_char_boundary(start) {
            start -= 1;
        }
        while end < content.len() && !content.is_char_boundary(end) {
            end += 1;
        }

        // Find word boundaries
        let start = if start > 0 && start < content.len() {
            // Search backwards from start for whitespace
            let search_slice = &content[..start];
            search_slice
                .rfind(char::is_whitespace)
                .map(|i| i + 1)
                .unwrap_or(start)
        } else {
            0
        };

        let end = if end < content.len() {
            // Search forwards from end for whitespace
            let search_start = end;
            let search_slice = &content[search_start..];
            search_slice
                .find(char::is_whitespace)
                .map(|i| search_start + i)
                .unwrap_or(end)
        } else {
            content.len()
        };

        // Final boundary check for the adjusted positions
        let mut start = start;
        while start > 0 && !content.is_char_boundary(start) {
            start -= 1;
        }

        let mut end = end;
        while end < content.len() && !content.is_char_boundary(end) {
            end += 1;
        }

        let mut snippet = String::new();
        if start > 0 {
            snippet.push_str("...");
        }
        snippet.push_str(&content[start..end]);
        if end < content.len() {
            snippet.push_str("...");
        }

        snippet
    } else {
        // No match found, return beginning of content
        // Use char boundary-aware truncation
        let mut end = content.len().min(max_length);

        // Find the nearest char boundary if we're not at one
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }

        let mut snippet = content[..end].to_string();
        if end < content.len() {
            snippet.push_str("...");
        }
        snippet
    }
}

/// Process search results with score adjustments and progressive truncation
pub fn process_search_results(
    mut scored_messages: Vec<ScoredMessage>,
    query: &str,
    limit: usize,
) -> Vec<ScoredMessage> {
    // Adjust scores based on content type
    for sm in &mut scored_messages {
        sm.score = adjust_message_score(&sm.message, sm.score);
    }

    // Re-sort by adjusted scores
    scored_messages.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to limit
    scored_messages.truncate(limit);

    // Apply progressive truncation to content
    for (i, sm) in scored_messages.iter_mut().enumerate() {
        let content = sm.message.display_content();

        // First 2 results: full content
        // Next 3: up to 500 chars with snippet
        // Rest: up to 200 chars with snippet
        let _truncated_content = if i < 2 {
            // Keep full content for top results
            content.clone()
        } else if i < 5 {
            extract_snippet(&content, query, 500)
        } else {
            extract_snippet(&content, query, 200)
        };

        // For now, we keep the full message intact
        // The search tool will handle truncation when displaying
    }

    scored_messages
}

/// Process constellation search results with score adjustments
pub fn process_constellation_results(
    mut scored_messages: Vec<ScoredConstellationMessage>,
    _query: &str,
    limit: usize,
) -> Vec<ScoredConstellationMessage> {
    // Adjust scores based on content type
    for scm in &mut scored_messages {
        scm.score = adjust_message_score(&scm.message, scm.score);
    }

    // Re-sort by adjusted scores
    scored_messages.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to limit
    scored_messages.truncate(limit);

    scored_messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_snippet() {
        let content = "This is a long piece of text that contains the word pattern somewhere in the middle and continues on for a while after that.";
        let query = "pattern";

        let snippet = extract_snippet(content, query, 80);
        assert!(snippet.contains("pattern"));
        assert!(snippet.starts_with("..."));
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn test_adjust_score_for_tool_response() {
        let msg = Message::agent(MessageContent::ToolResponses(vec![]));
        let adjusted = adjust_message_score(&msg, 1.0);
        assert_eq!(adjusted, 0.7);
    }
}
