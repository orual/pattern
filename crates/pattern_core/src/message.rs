use chrono::{DateTime, Utc};
use genai::{ModelIden, chat::Usage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::agent::{SnowflakePosition, get_next_message_position_sync};
use crate::{MessageId, UserId};

// Conversions to/from genai types
mod conversions;

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

/// A batch of messages representing a complete request/response cycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBatch {
    /// ID of this batch (same as first message's position)
    pub id: SnowflakePosition,

    /// Type of batch
    pub batch_type: BatchType,

    /// Messages in this batch, ordered by sequence_num
    pub messages: Vec<Message>,

    /// Whether this batch is complete (no pending tool calls, etc)
    pub is_complete: bool,

    /// Parent batch ID if this is a continuation
    pub parent_batch_id: Option<SnowflakePosition>,

    /// Tool calls we're waiting for responses to
    #[serde(skip_serializing_if = "std::collections::HashSet::is_empty", default)]
    pending_tool_calls: std::collections::HashSet<String>,

    /// Notification for when all tool calls are paired (not serialized)
    #[serde(skip)]
    tool_pairing_notify: std::sync::Arc<tokio::sync::Notify>,
}

impl MessageBatch {
    /// Get the next sequence number for this batch
    pub fn next_sequence_num(&self) -> u32 {
        self.messages.len() as u32
    }

    /// Sort messages by sequence_num, falling back to position, then created_at
    fn sort_messages(&mut self) {
        self.messages.sort_by(|a, b| {
            // Try sequence_num first
            match (&a.sequence_num, &b.sequence_num) {
                (Some(a_seq), Some(b_seq)) => a_seq.cmp(&b_seq),
                _ => {
                    // Fall back to position if either is None
                    match (&a.position, &b.position) {
                        (Some(a_pos), Some(b_pos)) => a_pos.cmp(&b_pos),
                        _ => {
                            // Last resort: created_at (always present)
                            a.created_at.cmp(&b.created_at)
                        }
                    }
                }
            }
        });
    }
    /// Create a new batch starting with a user message
    pub fn new_user_request(content: impl Into<MessageContent>) -> Self {
        let batch_id = get_next_message_position_sync();
        let mut message = Message::user(content);

        // Update message with batch info
        message.position = Some(batch_id);
        message.batch = Some(batch_id);
        message.sequence_num = Some(0);
        message.batch_type = Some(BatchType::UserRequest);

        let mut batch = Self {
            id: batch_id,
            batch_type: BatchType::UserRequest,
            messages: vec![],
            is_complete: false,
            parent_batch_id: None,
            pending_tool_calls: std::collections::HashSet::new(),
            tool_pairing_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        };

        // Track any tool calls in the message
        batch.track_message_tools(&message);
        batch.messages.push(message);
        batch
    }

    /// Create a system-triggered batch
    pub fn new_system_trigger(content: impl Into<MessageContent>) -> Self {
        let batch_id = get_next_message_position_sync();
        let mut message = Message::user(content); // compatibility with anthropic,
        // consider more intelligent way to do this

        message.position = Some(batch_id);
        message.batch = Some(batch_id);
        message.sequence_num = Some(0);
        message.batch_type = Some(BatchType::SystemTrigger);

        let mut batch = Self {
            id: batch_id,
            batch_type: BatchType::SystemTrigger,
            messages: vec![],
            is_complete: false,
            parent_batch_id: None,
            pending_tool_calls: std::collections::HashSet::new(),
            tool_pairing_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        };

        batch.track_message_tools(&message);
        batch.messages.push(message);
        batch
    }

    /// Create a continuation batch
    pub fn continuation(parent_batch_id: SnowflakePosition) -> Self {
        let batch_id = get_next_message_position_sync();

        Self {
            id: batch_id,
            batch_type: BatchType::Continuation,
            messages: Vec::new(),
            is_complete: false,
            parent_batch_id: Some(parent_batch_id),
            pending_tool_calls: std::collections::HashSet::new(),
            tool_pairing_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Add a message to this batch
    pub fn add_message(&mut self, mut message: Message) -> Message {
        // Ensure batch is sorted
        self.sort_messages();

        // Check if this message contains tool responses that should be sequenced
        match &message.content {
            MessageContent::ToolResponses(responses) => {
                // Check if all responses match tool calls at the end of current messages
                // This handles the 99% case where tool responses immediately follow their calls
                let all_match_at_end = self.check_responses_match_end(responses);

                if all_match_at_end {
                    // Simple case: tool responses are already in order, just append the message
                    // This preserves the original message ID and all fields
                    if message.position.is_none() {
                        message.position = Some(get_next_message_position_sync());
                    }
                    if message.batch.is_none() {
                        message.batch = Some(self.id);
                    }
                    if message.sequence_num.is_none() {
                        message.sequence_num = Some(self.messages.len() as u32);
                    }
                    if message.batch_type.is_none() {
                        message.batch_type = Some(self.batch_type);
                    }

                    // Update pending tool calls
                    for response in responses {
                        self.pending_tool_calls.remove(&response.call_id);
                    }

                    // Track and add the message
                    self.track_message_tools(&message);
                    self.messages.push(message.clone());

                    // Check if batch is complete
                    if self.pending_tool_calls.is_empty() {
                        self.tool_pairing_notify.notify_waiters();
                    }

                    return message;
                } else {
                    // Complex case: responses need reordering, use existing logic
                    let mut last_message = None;
                    for response in responses.clone() {
                        if let Some(msg) = self.add_tool_response_with_sequencing(response) {
                            last_message = Some(msg);
                        }
                    }
                    // Return the last inserted message or the original if none were inserted
                    return last_message.unwrap_or(message);
                }
            }
            MessageContent::Blocks(blocks) => {
                // Check if blocks contain tool results that need sequencing
                let tool_results: Vec<_> = blocks
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = block
                        {
                            Some(ToolResponse {
                                call_id: tool_use_id.clone(),
                                content: content.clone(),
                                is_error: None,
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                if !tool_results.is_empty() {
                    // Check if tool results match calls at the end
                    let all_match_at_end = self.check_responses_match_end(&tool_results);

                    if all_match_at_end
                        && !blocks
                            .iter()
                            .any(|b| !matches!(b, ContentBlock::ToolResult { .. }))
                    {
                        // Simple case: only tool results and they're in order
                        // Just append the whole message as-is
                        if message.position.is_none() {
                            message.position = Some(get_next_message_position_sync());
                        }
                        if message.batch.is_none() {
                            message.batch = Some(self.id);
                        }
                        if message.sequence_num.is_none() {
                            message.sequence_num = Some(self.messages.len() as u32);
                        }
                        if message.batch_type.is_none() {
                            message.batch_type = Some(self.batch_type);
                        }

                        // Update pending tool calls
                        for response in &tool_results {
                            self.pending_tool_calls.remove(&response.call_id);
                        }

                        // Track and add the message
                        self.track_message_tools(&message);
                        self.messages.push(message.clone());

                        // Check if batch is complete
                        if self.pending_tool_calls.is_empty() {
                            self.tool_pairing_notify.notify_waiters();
                        }

                        return message;
                    } else {
                        // Complex case: mixed content or needs reordering
                        let mut last_response_msg = None;
                        for response in tool_results {
                            if let Some(msg) = self.add_tool_response_with_sequencing(response) {
                                last_response_msg = Some(msg);
                            }
                        }

                        // Also add any non-tool-result blocks as a regular message
                        let non_tool_blocks: Vec<_> = blocks
                            .iter()
                            .filter_map(|block| {
                                if !matches!(block, ContentBlock::ToolResult { .. }) {
                                    Some(block.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        if !non_tool_blocks.is_empty() {
                            let mut new_msg = message.clone();
                            new_msg.content = MessageContent::Blocks(non_tool_blocks);
                            // Recursively add the non-tool blocks (will hit the default path below)
                            let updated_msg = self.add_message(new_msg);
                            return updated_msg;
                        }

                        // Tool results were processed separately - return the last message added to batch
                        return last_response_msg.unwrap_or(message);
                    }
                }
            }
            _ => {}
        }

        // Default path for regular messages and tool calls
        // Only set batch fields if they're not already set
        if message.position.is_none() {
            message.position = Some(get_next_message_position_sync());
        }
        if message.batch.is_none() {
            message.batch = Some(self.id);
        }
        if message.sequence_num.is_none() {
            message.sequence_num = Some(self.messages.len() as u32);
        }
        if message.batch_type.is_none() {
            message.batch_type = Some(self.batch_type);
        }

        // Track tool calls/responses
        self.track_message_tools(&message);

        self.messages.push(message.clone());

        // Notify waiters if all tool calls are paired
        if self.pending_tool_calls.is_empty() {
            self.tool_pairing_notify.notify_waiters();
        }

        message
    }

    /// Add an agent response to this batch
    pub fn add_agent_response(&mut self, content: impl Into<MessageContent>) -> Message {
        // Ensure batch is sorted
        self.sort_messages();

        let sequence_num = self.messages.len() as u32;
        let mut message = Message::assistant_in_batch(self.id, sequence_num, content);
        message.batch_type = Some(self.batch_type);
        self.add_message(message)
    }

    /// Add tool responses to this batch
    pub fn add_tool_responses(&mut self, responses: Vec<ToolResponse>) -> Message {
        // Ensure batch is sorted
        self.sort_messages();

        let sequence_num = self.messages.len() as u32;
        let mut message = Message::tool_in_batch(self.id, sequence_num, responses);
        message.batch_type = Some(self.batch_type);
        self.add_message(message)
    }

    /// Add multiple tool responses, inserting them after their corresponding calls
    /// and resequencing subsequent messages
    pub fn add_tool_responses_with_sequencing(&mut self, responses: Vec<ToolResponse>) -> Message {
        // Ensure batch is sorted
        self.sort_messages();

        // Sort responses by the position of their corresponding calls
        // This ensures we process them in the right order to minimize resequencing
        let mut responses_with_positions: Vec<(Option<usize>, ToolResponse)> = responses
            .into_iter()
            .map(|r| {
                let pos = self.find_tool_call_position(&r.call_id);
                (pos, r)
            })
            .collect();

        // Sort by position (None goes last)
        responses_with_positions.sort_by_key(|(pos, _)| pos.unwrap_or(usize::MAX));

        let mut msg = None;
        let mut resp_pos = self.messages.len();
        // Process each response
        for (call_pos, response) in responses_with_positions {
            if let Some(pos) = call_pos {
                msg = Some(self.insert_tool_response_at(pos, response));
                resp_pos = pos + 1;
            } else {
                tracing::debug!(
                    "Received tool response with call_id {} but no matching tool call found in batch",
                    response.call_id
                );
            }
        }

        // Renumber all messages after insertions
        for (idx, msg) in self.messages.iter_mut().enumerate() {
            msg.sequence_num = Some(idx as u32);
        }

        if let Some(ref mut msg) = msg {
            msg.sequence_num = Some(resp_pos as u32);
        }

        // Notify waiters if all tool calls are paired
        if self.pending_tool_calls.is_empty() {
            self.tool_pairing_notify.notify_waiters();
        }
        msg.unwrap_or_else(|| Message::system("Tool responses processed"))
    }

    /// Helper to insert a tool response after its corresponding call
    fn insert_tool_response_at(&mut self, call_pos: usize, response: ToolResponse) -> Message {
        let insert_pos = call_pos + 1;

        // Check if we can append to an existing ToolResponses message at insert_pos
        if insert_pos < self.messages.len() {
            if let MessageContent::ToolResponses(existing_responses) =
                &mut self.messages[insert_pos].content
            {
                // Append to existing tool responses
                if self.pending_tool_calls.contains(&response.call_id) {
                    existing_responses.push(response.clone());
                    self.pending_tool_calls.remove(&response.call_id);
                }
                return self.messages[insert_pos].clone();
            }
        }

        // Create a new tool response message
        let mut response_msg = Message::tool(vec![response.clone()]);

        // Set batch fields
        let position = get_next_message_position_sync();
        response_msg.position = Some(position);
        response_msg.batch = Some(self.id);
        response_msg.sequence_num = Some(insert_pos as u32);
        response_msg.batch_type = Some(self.batch_type);

        // Insert the response message
        self.messages.insert(insert_pos, response_msg.clone());

        // Update tracking
        self.pending_tool_calls.remove(&response.call_id);

        response_msg
    }

    /// Add a single tool response, inserting it immediately after the corresponding call
    /// and resequencing subsequent messages
    pub fn add_tool_response_with_sequencing(&mut self, response: ToolResponse) -> Option<Message> {
        // Ensure batch is sorted
        self.sort_messages();

        // Find the message containing the matching tool call
        let call_position = self.find_tool_call_position(&response.call_id);

        if let Some(call_pos) = call_position {
            let mut inserted_message = self.insert_tool_response_at(call_pos, response);
            let insert_pos = call_pos + 1;

            // Renumber all messages after insertions
            for (idx, msg) in self.messages.iter_mut().enumerate() {
                msg.sequence_num = Some(idx as u32);
            }

            // Update the returned message's sequence number to match what it got renumbered to
            inserted_message.sequence_num = Some(insert_pos as u32);

            // Check if batch is now complete
            if self.pending_tool_calls.is_empty() {
                self.tool_pairing_notify.notify_waiters();
            }

            Some(inserted_message)
        } else {
            // No matching tool call found - this is an error condition
            // Log it but don't add an unpaired response
            tracing::debug!(
                "Received tool response with call_id {} but no matching tool call found in batch",
                response.call_id
            );
            None
        }
    }

    /// Get a clone of the tool pairing notifier for async waiting
    pub fn get_tool_pairing_notifier(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.tool_pairing_notify.clone()
    }

    /// Find the position of the message containing a specific tool call
    fn find_tool_call_position(&self, call_id: &str) -> Option<usize> {
        for (idx, msg) in self.messages.iter().enumerate() {
            match &msg.content {
                MessageContent::ToolCalls(calls) => {
                    if calls.iter().any(|c| c.call_id == call_id) {
                        return Some(idx);
                    }
                }
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolUse { id, .. } = block {
                            if id == call_id {
                                return Some(idx);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Check if batch has unpaired tool calls
    pub fn has_pending_tool_calls(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    /// Get the IDs of pending tool calls (for debugging/migration)
    pub fn get_pending_tool_calls(&self) -> Vec<String> {
        self.pending_tool_calls.iter().cloned().collect()
    }

    /// Mark batch as complete
    pub fn mark_complete(&mut self) {
        self.is_complete = true;
    }

    /// Finalize batch by removing unpaired tool calls and orphaned tool responses
    /// Returns the IDs of removed messages for cleanup
    pub fn finalize(&mut self) -> Vec<crate::id::MessageId> {
        let mut removed_ids = Vec::new();

        // First, collect all tool call IDs that have responses
        let mut responded_tool_calls = std::collections::HashSet::new();
        for msg in &self.messages {
            match &msg.content {
                MessageContent::ToolResponses(responses) => {
                    for resp in responses {
                        responded_tool_calls.insert(resp.call_id.clone());
                    }
                }
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            responded_tool_calls.insert(tool_use_id.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        // Track which messages to remove
        let mut indices_to_remove = Vec::new();

        // Remove unpaired tool calls
        if !self.pending_tool_calls.is_empty() {
            let pending = self.pending_tool_calls.clone();

            for (idx, msg) in self.messages.iter_mut().enumerate() {
                let should_remove_message = match &mut msg.content {
                    MessageContent::ToolCalls(calls) => {
                        // Remove entire message if all calls are unpaired
                        calls.iter().all(|call| pending.contains(&call.call_id))
                    }
                    MessageContent::Blocks(blocks) => {
                        // Filter out unpaired tool calls from blocks
                        let original_len = blocks.len();
                        blocks.retain(|block| {
                            !matches!(block, ContentBlock::ToolUse { id, .. } if pending.contains(id))
                        });

                        // If we removed tool calls and now the last block is Thinking,
                        // replace the entire content with a simple text message
                        if blocks.len() < original_len {
                            if let Some(ContentBlock::Thinking { .. }) = blocks.last() {
                                // Replace with empty assistant text to maintain message flow
                                msg.content = MessageContent::Text(String::new());
                                false // Don't remove the message
                            } else if blocks.is_empty() {
                                // If all blocks were removed, mark for deletion
                                true
                            } else {
                                false // Keep the message with filtered blocks
                            }
                        } else {
                            false // No changes needed
                        }
                    }
                    _ => false,
                };

                if should_remove_message {
                    indices_to_remove.push(idx);
                    removed_ids.push(msg.id.clone());
                }
            }
        }

        // Also remove orphaned tool responses (responses without matching calls)
        for (idx, msg) in self.messages.iter().enumerate() {
            if indices_to_remove.contains(&idx) {
                continue; // Already marked for removal
            }

            let should_remove = match &msg.content {
                MessageContent::ToolResponses(responses) => {
                    // Remove if all responses are orphaned
                    responses.iter().all(|resp| {
                        // A response is orphaned if there's no matching tool call in this batch
                        !self.messages.iter().any(|m| match &m.content {
                            MessageContent::ToolCalls(calls) => {
                                calls.iter().any(|call| call.call_id == resp.call_id)
                            }
                            MessageContent::Blocks(blocks) => {
                                blocks.iter().any(|block| {
                                    matches!(block, ContentBlock::ToolUse { id, .. } if id == &resp.call_id)
                                })
                            }
                            _ => false,
                        })
                    })
                }
                MessageContent::Blocks(blocks) => {
                    // Check if this is purely orphaned tool responses
                    let has_orphaned = blocks.iter().any(|block| {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            // Check if there's a matching tool call
                            !self.messages.iter().any(|m| match &m.content {
                                MessageContent::ToolCalls(calls) => {
                                    calls.iter().any(|call| &call.call_id == tool_use_id)
                                }
                                MessageContent::Blocks(inner_blocks) => {
                                    inner_blocks.iter().any(|b| {
                                        matches!(b, ContentBlock::ToolUse { id, .. } if id == tool_use_id)
                                    })
                                }
                                _ => false,
                            })
                        } else {
                            false
                        }
                    });
                    let has_other_content = blocks
                        .iter()
                        .any(|block| !matches!(block, ContentBlock::ToolResult { .. }));
                    // Remove if it only has orphaned tool responses
                    has_orphaned && !has_other_content
                }
                _ => false,
            };

            if should_remove {
                indices_to_remove.push(idx);
                removed_ids.push(msg.id.clone());
            }
        }

        // Remove messages by index in reverse order
        indices_to_remove.sort_unstable();
        indices_to_remove.dedup();
        for idx in indices_to_remove.into_iter().rev() {
            self.messages.remove(idx);
        }

        // Clear pending tool calls (but don't mark complete - caller should do that)
        self.pending_tool_calls.clear();

        // Renumber sequences after removal
        for (i, msg) in self.messages.iter_mut().enumerate() {
            msg.sequence_num = Some(i as u32);
        }

        // NOTE: Caller must explicitly call mark_complete() if desired
        // This allows cleanup without forcing completion

        removed_ids
    }

    /// Get the total number of messages in this batch
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if batch is empty
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Reconstruct a batch from existing messages (for migration/loading)
    pub fn from_messages(
        id: SnowflakePosition,
        batch_type: BatchType,
        messages: Vec<Message>,
    ) -> Self {
        let mut batch = Self {
            id,
            batch_type,
            messages: vec![],
            is_complete: false,
            parent_batch_id: None,
            pending_tool_calls: std::collections::HashSet::new(),
            tool_pairing_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        };

        // Add each message through add_message to ensure proper tool response sequencing
        for msg in messages {
            batch.add_message(msg);
        }

        // Check if complete: final message is tool responses or assistant message
        let last_is_assistant = batch
            .messages
            .last()
            .map(|m| m.role == ChatRole::Assistant || m.role == ChatRole::Tool)
            .unwrap_or(false);

        if batch.pending_tool_calls.is_empty() && last_is_assistant {
            batch.is_complete = true;
        }

        batch
    }

    /// Check if tool responses match tool calls at the end of the batch
    /// Returns true if all responses have matching calls and they're at the end
    fn check_responses_match_end(&self, responses: &[ToolResponse]) -> bool {
        if responses.is_empty() || self.messages.is_empty() {
            return false;
        }

        // Get all tool call IDs from the last few messages
        let mut recent_calls = std::collections::HashSet::new();

        // Look backwards through messages to find recent tool calls
        for msg in self.messages.iter().rev().take(5) {
            match &msg.content {
                MessageContent::ToolCalls(calls) => {
                    for call in calls {
                        recent_calls.insert(call.call_id.clone());
                    }
                }
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolUse { id, .. } = block {
                            recent_calls.insert(id.clone());
                        }
                    }
                }
                _ => {}
            }

            // If we found calls, stop looking
            if !recent_calls.is_empty() {
                break;
            }
        }

        // Check if all responses have matching calls
        responses
            .iter()
            .all(|resp| recent_calls.contains(&resp.call_id))
    }

    /// Track tool calls/responses in a message
    fn track_message_tools(&mut self, message: &Message) {
        match &message.content {
            MessageContent::ToolCalls(calls) => {
                for call in calls {
                    self.pending_tool_calls.insert(call.call_id.clone());
                }
            }
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::ToolUse { id, .. } => {
                            self.pending_tool_calls.insert(id.clone());
                        }
                        ContentBlock::ToolResult { tool_use_id, .. } => {
                            self.pending_tool_calls.remove(tool_use_id);
                        }
                        _ => {}
                    }
                }
            }
            MessageContent::ToolResponses(responses) => {
                for response in responses {
                    self.pending_tool_calls.remove(&response.call_id);
                }
            }
            _ => {}
        }
    }

    /// Wait for all pending tool calls to be paired with responses
    pub async fn wait_for_tool_pairing(&self) {
        while !self.pending_tool_calls.is_empty() {
            tracing::info!("batch {} has no more pending tool calls", self.id);
            self.tool_pairing_notify.notified().await;
        }
    }

    /// Check if a specific tool call is pending
    pub fn is_waiting_for(&self, call_id: &str) -> bool {
        self.pending_tool_calls.contains(call_id)
    }
}

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

/// A response generated by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub system: Option<Vec<String>>,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<genai::chat::Tool>>,
}

impl Request {
    /// Convert this request to a genai ChatRequest
    pub fn as_chat_request(&mut self) -> crate::Result<genai::chat::ChatRequest> {
        // Fix assistant messages that end with thinking blocks
        for msg in &mut self.messages {
            if msg.role == ChatRole::User || msg.role == ChatRole::System {
                if let MessageContent::Text(text) = &msg.content {
                    use chrono::TimeZone;
                    let time_zone = chrono::Local::now().timezone();
                    let timestamp = time_zone.from_utc_datetime(&msg.created_at.naive_utc());
                    // injecting created time in to make agents less likely to be confused by artifacts and more temporally aware.
                    msg.content = MessageContent::Text(format!(
                        "<time_sync>created: {}</time_sync>\n{}",
                        timestamp, text
                    ));
                }
            } else if msg.role == ChatRole::Assistant {
                if let MessageContent::Blocks(blocks) = &mut msg.content {
                    if let Some(last_block) = blocks.last() {
                        // Check if the last block is a thinking block
                        let ends_with_thinking = matches!(
                            last_block,
                            ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                        );

                        if ends_with_thinking {
                            // Append a minimal text block to fix the issue
                            tracing::debug!(
                                "Appending text block after thinking block in assistant message"
                            );
                            blocks.push(ContentBlock::Text {
                                text: ".".to_string(), // Single period to satisfy non-empty requirement
                                thought_signature: None,
                            });
                        }
                    }
                }
            }
        }

        let messages: Vec<_> = self
            .messages
            .iter()
            .filter(|m| Message::estimate_word_count(&m.content) > 0)
            .map(|m| m.as_chat_message())
            .collect();

        Ok(
            genai::chat::ChatRequest::from_system(self.system.clone().unwrap().join("\n\n"))
                .append_messages(messages)
                .with_tools(self.tools.clone().unwrap_or_default()),
        )
    }
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

/// A response generated by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub content: Vec<MessageContent>,
    pub reasoning: Option<String>,
    pub metadata: ResponseMetadata,
}

impl Response {
    /// Create a Response from a genai ChatResponse
    pub fn from_chat_response(resp: genai::chat::ChatResponse) -> Self {
        // Extract data before consuming resp
        let reasoning = resp.reasoning_content.clone();
        let metadata = ResponseMetadata {
            processing_time: None,
            tokens_used: Some(resp.usage.clone()),
            model_used: Some(resp.provider_model_iden.to_string()),
            confidence: None,
            model_iden: resp.model_iden.clone(),
            custom: resp.captured_raw_body.clone().unwrap_or_default(),
        };

        // Convert genai MessageContent to our MessageContent
        let content: Vec<MessageContent> = resp
            .content
            .clone()
            .into_iter()
            .map(|gc| gc.into())
            .collect();

        Self {
            content,
            reasoning,
            metadata,
        }
    }

    pub fn num_tool_calls(&self) -> usize {
        self.content
            .iter()
            .filter(|c| c.tool_calls().is_some())
            .count()
    }

    pub fn num_tool_responses(&self) -> usize {
        self.content
            .iter()
            .filter(|c| match c {
                MessageContent::ToolResponses(_) => true,
                _ => false,
            })
            .count()
    }

    pub fn has_unpaired_tool_calls(&self) -> bool {
        // Collect all tool call IDs
        let mut tool_calls: Vec<String> = Vec::new();

        // Get tool calls from ToolCalls content
        for content in &self.content {
            if let MessageContent::ToolCalls(calls) = content {
                for call in calls {
                    tool_calls.push(call.call_id.clone());
                }
            }
        }

        // Get tool calls from Blocks
        for content in &self.content {
            if let MessageContent::Blocks(blocks) = content {
                for block in blocks {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        tool_calls.push(id.clone());
                    }
                }
            }
        }

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            return false;
        }

        // Check if we have Anthropic-style IDs (start with "toolu_")
        let has_anthropic_ids = tool_calls.iter().any(|id| id.starts_with("toolu_"));

        if has_anthropic_ids {
            // Anthropic IDs are unique - use set difference
            let tool_call_set: std::collections::HashSet<String> = tool_calls.into_iter().collect();

            let mut tool_response_set: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Get tool responses from ToolResponses content
            for content in &self.content {
                if let MessageContent::ToolResponses(responses) = content {
                    for response in responses {
                        tool_response_set.insert(response.call_id.clone());
                    }
                }
            }

            // Get tool responses from Blocks
            for content in &self.content {
                if let MessageContent::Blocks(blocks) = content {
                    for block in blocks {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            tool_response_set.insert(tool_use_id.clone());
                        }
                    }
                }
            }

            // Check if there are any tool calls without responses
            tool_call_set.difference(&tool_response_set).count() > 0
        } else {
            // Gemini/other IDs may not be unique - count occurrences
            use std::collections::HashMap;
            let mut call_counts: HashMap<String, usize> = HashMap::new();

            // Count tool calls
            for id in tool_calls {
                *call_counts.entry(id).or_insert(0) += 1;
            }

            // Subtract tool responses
            for content in &self.content {
                if let MessageContent::ToolResponses(responses) = content {
                    for response in responses {
                        if let Some(count) = call_counts.get_mut(&response.call_id) {
                            *count = count.saturating_sub(1);
                        }
                    }
                }
            }

            // Subtract tool responses from Blocks
            for content in &self.content {
                if let MessageContent::Blocks(blocks) = content {
                    for block in blocks {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                            if let Some(count) = call_counts.get_mut(tool_use_id) {
                                *count = count.saturating_sub(1);
                            }
                        }
                    }
                }
            }

            // Check if any tool calls remain unpaired
            call_counts.values().any(|&count| count > 0)
        }
    }

    pub fn only_text(&self) -> String {
        let mut text = String::new();
        for content in &self.content {
            match content {
                MessageContent::Text(txt) => text.push_str(txt),
                MessageContent::Parts(content_parts) => {
                    for part in content_parts {
                        match part {
                            ContentPart::Text(txt) => text.push_str(txt),
                            ContentPart::Image { .. } => {}
                        }
                        text.push('\n');
                    }
                }
                MessageContent::ToolCalls(_) => {}
                MessageContent::ToolResponses(_) => {}
                MessageContent::Blocks(_) => {}
            }
            text.push('\n');
        }
        text
    }
}

/// Metadata for a response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time: Option<chrono::Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub model_iden: ModelIden,
    pub custom: serde_json::Value,
}

impl Default for ResponseMetadata {
    fn default() -> Self {
        Self {
            processing_time: None,
            tokens_used: None,
            model_used: None,
            confidence: None,
            custom: json!({}),
            model_iden: ModelIden::new(genai::adapter::AdapterKind::Ollama, "default_model"),
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
        msg.position = Some(crate::agent::get_next_message_position_sync());
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
        msg.position = Some(crate::agent::get_next_message_position_sync());
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

                        let position = crate::agent::get_next_message_position_sync();

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
                    let position = crate::agent::get_next_message_position_sync();

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

            let position = crate::agent::get_next_message_position_sync();

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
