use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages::{ChatRole, ContentBlock, Message, MessageContent, ToolResponse};
use crate::{SnowflakePosition, utils::get_next_message_position_sync};

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
