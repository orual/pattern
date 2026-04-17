// MOVING TO: pattern_runtime/src/agent/collect.rs
// ORIGIN: crates/pattern_core/src/agent/collect.rs
// PHASE: 3
// RESHAPE: Helper for agent message collection; reshape during runtime integration
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Response collection utilities for stream-based agent processing

use futures::StreamExt;
use tokio_stream::Stream;

use crate::agent::ResponseEvent;
use crate::error::CoreError;
use crate::messages::{MessageContent, Response};

/// Collect a stream of ResponseEvents into a final Response
///
/// This helper aggregates streaming events into a complete Response,
/// useful for callers who don't need real-time streaming.
pub async fn collect_response(
    mut stream: impl Stream<Item = ResponseEvent> + Unpin,
) -> Result<Response, CoreError> {
    let mut content = Vec::new();
    let mut reasoning = None;
    let mut metadata = None;

    while let Some(event) = stream.next().await {
        match event {
            ResponseEvent::TextChunk {
                text,
                is_final: true,
            } => {
                content.push(MessageContent::Text(text));
            }
            ResponseEvent::TextChunk {
                text,
                is_final: false,
            } => {
                // Accumulate partial chunks - for now just take finals
                let _ = text;
            }
            ResponseEvent::ReasoningChunk {
                text,
                is_final: true,
            } => {
                reasoning = Some(text);
            }
            ResponseEvent::ReasoningChunk {
                text,
                is_final: false,
            } => {
                let _ = text;
            }
            ResponseEvent::ToolCalls { calls } => {
                content.push(MessageContent::ToolCalls(calls));
            }
            ResponseEvent::ToolResponses { responses } => {
                content.push(MessageContent::ToolResponses(responses));
            }
            ResponseEvent::Complete { metadata: meta, .. } => {
                metadata = Some(meta);
            }
            ResponseEvent::Error { message, .. } => {
                return Err(CoreError::AgentProcessing {
                    agent_id: "unknown".to_string(),
                    details: message,
                });
            }
            _ => {}
        }
    }

    Ok(Response {
        content,
        reasoning,
        metadata: metadata.unwrap_or_default(),
    })
}
