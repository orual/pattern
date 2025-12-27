//! MessageStore: Per-agent message operations wrapper.
//!
//! Provides a scoped interface for message storage, retrieval, and coordination
//! operations. Each MessageStore is bound to a specific agent and delegates to
//! pattern_db query modules.

use pattern_db::error::DbResult;
use pattern_db::models::{self, ActivityEvent, AgentSummary, ArchiveSummary, MessageSummary};
use sqlx::SqlitePool;

use crate::agent::SnowflakePosition;
use crate::error::CoreError;
use crate::id::MessageId;
use crate::message::{
    self, ChatRole, ContentPart, Message, MessageContent, MessageMetadata, MessageOptions,
};
use std::str::FromStr;

/// Extract text preview from MessageContent for FTS indexing
fn extract_content_preview(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(text) => Some(text.clone()),
        MessageContent::Parts(parts) => {
            // Pre-calculate approximate capacity to reduce allocations
            let estimated_len: usize = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text(t) => Some(t.len()),
                    _ => None,
                })
                .sum();

            if estimated_len == 0 {
                return None;
            }

            let mut result = String::with_capacity(estimated_len + parts.len());
            let mut first = true;
            for part in parts {
                if let ContentPart::Text(t) = part {
                    if !first {
                        result.push('\n');
                    }
                    result.push_str(t);
                    first = false;
                }
            }
            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        }
        MessageContent::ToolResponses(responses) => {
            let estimated_len: usize = responses.iter().map(|r| r.content.len()).sum();
            if estimated_len == 0 {
                return None;
            }

            let mut result = String::with_capacity(estimated_len + responses.len());
            let mut first = true;
            for response in responses {
                if !first {
                    result.push('\n');
                }
                result.push_str(&response.content);
                first = false;
            }
            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        }
        MessageContent::Blocks(blocks) => {
            use crate::message::ContentBlock;
            let estimated_len: usize = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.len()),
                    ContentBlock::Thinking { text, .. } => Some(text.len()),
                    _ => None,
                })
                .sum();

            if estimated_len == 0 {
                return None;
            }

            let mut result = String::with_capacity(estimated_len + blocks.len());
            let mut first = true;
            for block in blocks {
                match block {
                    ContentBlock::Text { text, .. } | ContentBlock::Thinking { text, .. } => {
                        if !first {
                            result.push('\n');
                        }
                        result.push_str(text);
                        first = false;
                    }
                    _ => {}
                }
            }
            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        }
        _ => None,
    }
}

/// Convert database Message to domain Message
fn db_message_to_domain(db_msg: models::Message) -> Result<Message, CoreError> {
    // Convert role
    let role = match db_msg.role {
        models::MessageRole::User => ChatRole::User,
        models::MessageRole::Assistant => ChatRole::Assistant,
        models::MessageRole::System => ChatRole::System,
        models::MessageRole::Tool => ChatRole::Tool,
    };

    // Deserialize content from JSON
    let content: MessageContent =
        serde_json::from_value(db_msg.content_json.0.clone()).map_err(|e| {
            CoreError::SerializationError {
                data_type: "MessageContent".to_string(),
                cause: e,
            }
        })?;

    // Convert metadata from source_metadata JSON
    let metadata = if let Some(source_metadata) = &db_msg.source_metadata {
        serde_json::from_value(source_metadata.0.clone()).map_err(|e| {
            CoreError::SerializationError {
                data_type: "MessageMetadata".to_string(),
                cause: e,
            }
        })?
    } else {
        MessageMetadata {
            timestamp: Some(db_msg.created_at),
            ..Default::default()
        }
    };

    // Parse position from string
    let position =
        SnowflakePosition::from_str(&db_msg.position).map_err(|e| CoreError::InvalidFormat {
            data_type: "SnowflakePosition".to_string(),
            details: format!("Failed to parse position '{}': {}", db_msg.position, e),
        })?;

    // Parse batch_id if present
    let batch = db_msg
        .batch_id
        .as_ref()
        .map(|s| SnowflakePosition::from_str(s))
        .transpose()
        .map_err(|e| CoreError::InvalidFormat {
            data_type: "SnowflakePosition".to_string(),
            details: format!("Failed to parse batch_id: {}", e),
        })?;

    // Parse batch_type if present
    let batch_type = db_msg.batch_type.map(|bt| match bt {
        models::BatchType::UserRequest => message::BatchType::UserRequest,
        models::BatchType::AgentToAgent => message::BatchType::AgentToAgent,
        models::BatchType::SystemTrigger => message::BatchType::SystemTrigger,
        models::BatchType::Continuation => message::BatchType::Continuation,
    });

    // Compute has_tool_calls
    let has_tool_calls = matches!(content, MessageContent::ToolCalls(_))
        || matches!(content, MessageContent::Blocks(ref blocks) if blocks.iter().any(|b| matches!(b, crate::message::ContentBlock::ToolUse { .. })));

    // Compute word_count - count words in content
    let word_count = if let Some(preview) = &db_msg.content_preview {
        preview.split_whitespace().count() as u32
    } else {
        0
    };

    Ok(Message {
        id: MessageId(db_msg.id),
        role,
        owner_id: None, // Database doesn't track owner_id currently
        content,
        metadata,
        options: MessageOptions::default(),
        has_tool_calls,
        word_count,
        created_at: db_msg.created_at,
        position: Some(position),
        batch,
        sequence_num: db_msg.sequence_in_batch.map(|n| n as u32),
        batch_type,
    })
}

/// Convert domain Message to database Message for storage
fn domain_message_to_db(agent_id: String, msg: &Message) -> Result<models::Message, CoreError> {
    let role = match msg.role {
        ChatRole::User => models::MessageRole::User,
        ChatRole::Assistant => models::MessageRole::Assistant,
        ChatRole::System => models::MessageRole::System,
        ChatRole::Tool => models::MessageRole::Tool,
    };

    // Serialize content to JSON
    let content_json =
        serde_json::to_value(&msg.content).map_err(|e| CoreError::SerializationError {
            data_type: "MessageContent".to_string(),
            cause: e,
        })?;

    // Extract text preview for FTS
    let content_preview = extract_content_preview(&msg.content);

    // Serialize batch_type
    let batch_type = msg.batch_type.map(|bt| match bt {
        message::BatchType::UserRequest => models::BatchType::UserRequest,
        message::BatchType::AgentToAgent => models::BatchType::AgentToAgent,
        message::BatchType::SystemTrigger => models::BatchType::SystemTrigger,
        message::BatchType::Continuation => models::BatchType::Continuation,
    });

    // Serialize metadata - propagate errors instead of swallowing with .ok()
    let source_metadata = serde_json::to_value(&msg.metadata)
        .map(|v| Some(sqlx::types::Json(v)))
        .map_err(|e| CoreError::SerializationError {
            data_type: "MessageMetadata".to_string(),
            cause: e,
        })?;

    // Extract source from metadata custom fields if it exists
    let source = msg
        .metadata
        .custom
        .get("source")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(models::Message {
        id: msg.id.0.clone(),
        agent_id,
        position: msg
            .position
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        batch_id: msg.batch.as_ref().map(|b| b.to_string()),
        sequence_in_batch: msg.sequence_num.map(|n| n as i64),
        role,
        content_json: sqlx::types::Json(content_json),
        content_preview,
        batch_type,
        source,
        source_metadata,
        is_archived: false,
        is_deleted: false,
        created_at: msg.metadata.timestamp.unwrap_or_else(chrono::Utc::now),
    })
}

/// Per-agent message store.
///
/// Wraps pattern_db query modules to provide agent-scoped message operations,
/// including message CRUD, batching, archival, summaries, and activity logging.
#[derive(Debug, Clone)]
pub struct MessageStore {
    pool: SqlitePool,
    agent_id: String,
}

impl MessageStore {
    /// Create a new MessageStore for a specific agent.
    ///
    /// # Arguments
    /// * `pool` - Database connection pool
    /// * `agent_id` - Agent identifier to scope operations to
    pub fn new(pool: SqlitePool, agent_id: impl Into<String>) -> Self {
        Self {
            pool,
            agent_id: agent_id.into(),
        }
    }

    // ============================================================================
    // Message Operations
    // ============================================================================

    /// Get a message by ID.
    pub async fn get_message(&self, id: &str) -> Result<Option<Message>, CoreError> {
        let db_msg = pattern_db::queries::get_message(&self.pool, id).await?;
        db_msg.map(db_message_to_domain).transpose()
    }

    /// Get recent non-archived messages.
    ///
    /// Returns up to `limit` messages ordered by position (newest first).
    pub async fn get_recent(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        let db_messages =
            pattern_db::queries::get_messages(&self.pool, &self.agent_id, limit as i64).await?;
        db_messages.into_iter().map(db_message_to_domain).collect()
    }

    /// Get all messages including archived.
    ///
    /// Returns up to `limit` messages ordered by position (newest first).
    pub async fn get_all(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        let db_messages = pattern_db::queries::get_messages_with_archived(
            &self.pool,
            &self.agent_id,
            limit as i64,
        )
        .await?;
        db_messages.into_iter().map(db_message_to_domain).collect()
    }

    /// Get messages after a specific position.
    ///
    /// Useful for pagination or catching up on new messages.
    pub async fn get_after(
        &self,
        after_position: &str,
        limit: usize,
    ) -> Result<Vec<Message>, CoreError> {
        let db_messages = pattern_db::queries::get_messages_after(
            &self.pool,
            &self.agent_id,
            after_position,
            limit as i64,
        )
        .await?;
        db_messages.into_iter().map(db_message_to_domain).collect()
    }

    /// Store a new message.
    pub async fn store(&self, message: &Message) -> Result<(), CoreError> {
        let db_msg = domain_message_to_db(self.agent_id.clone(), message)?;
        pattern_db::queries::create_message(&self.pool, &db_msg).await?;
        Ok(())
    }

    /// Archive messages before a specific position.
    ///
    /// Marks messages as archived without deleting them.
    /// Returns the number of messages archived.
    pub async fn archive_before(&self, position: &str) -> DbResult<u64> {
        pattern_db::queries::archive_messages(&self.pool, &self.agent_id, position).await
    }

    /// Hard delete messages before a specific position.
    ///
    /// **WARNING**: This permanently deletes messages. Use with caution.
    /// Returns the number of messages deleted.
    pub async fn delete_before(&self, position: &str) -> DbResult<u64> {
        pattern_db::queries::delete_messages(&self.pool, &self.agent_id, position).await
    }

    /// Count non-archived messages.
    pub async fn count(&self) -> DbResult<i64> {
        pattern_db::queries::count_messages(&self.pool, &self.agent_id).await
    }

    /// Count all messages including archived.
    pub async fn count_all(&self) -> DbResult<i64> {
        pattern_db::queries::count_all_messages(&self.pool, &self.agent_id).await
    }

    /// Get lightweight message summaries for listing.
    pub async fn get_summaries(&self, limit: usize) -> DbResult<Vec<MessageSummary>> {
        pattern_db::queries::get_message_summaries(&self.pool, &self.agent_id, limit as i64).await
    }

    // ============================================================================
    // Batch Operations
    // ============================================================================

    /// Get all messages in a specific batch.
    ///
    /// Returns messages ordered by sequence within the batch.
    pub async fn get_batch(&self, batch_id: &str) -> Result<Vec<Message>, CoreError> {
        let db_messages = pattern_db::queries::get_batch_messages(&self.pool, batch_id).await?;
        db_messages.into_iter().map(db_message_to_domain).collect()
    }

    /// Group messages by batch_id, preserving chronological order
    pub fn group_messages_by_batch(messages: Vec<Message>) -> Vec<Vec<Message>> {
        use std::collections::BTreeMap;

        // BTreeMap keeps batches ordered by SnowflakePosition (time-ordered)
        let mut batch_map: BTreeMap<Option<SnowflakePosition>, Vec<Message>> = BTreeMap::new();

        for msg in messages {
            let batch_id = msg.batch.clone();
            batch_map.entry(batch_id).or_default().push(msg);
        }

        // Sort messages within each batch by sequence_num
        for messages in batch_map.values_mut() {
            messages.sort_by_key(|m| m.sequence_num);
        }

        // Return batches in order (BTreeMap iteration is ordered)
        batch_map.into_values().collect()
    }

    /// Get messages as MessageBatches for compression
    pub async fn get_batches(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::message::MessageBatch>, CoreError> {
        let messages = self.get_recent(limit).await?;
        let grouped = Self::group_messages_by_batch(messages);

        let mut batches = Vec::new();
        for batch_messages in grouped {
            if let Some(first) = batch_messages.first() {
                if let Some(batch_id) = &first.batch {
                    let batch_type = first
                        .batch_type
                        .unwrap_or(crate::message::BatchType::UserRequest);
                    let batch = crate::message::MessageBatch::from_messages(
                        batch_id.clone(),
                        batch_type,
                        batch_messages,
                    );
                    batches.push(batch);
                }
            }
        }

        Ok(batches)
    }

    // ============================================================================
    // Archive Summaries
    // ============================================================================

    /// Get an archive summary by ID.
    pub async fn get_archive_summary(&self, id: &str) -> DbResult<Option<ArchiveSummary>> {
        pattern_db::queries::get_archive_summary(&self.pool, id).await
    }

    /// Get all archive summaries for this agent.
    pub async fn get_archive_summaries(&self) -> DbResult<Vec<ArchiveSummary>> {
        pattern_db::queries::get_archive_summaries(&self.pool, &self.agent_id).await
    }

    /// Create an archive summary.
    pub async fn create_archive_summary(&self, summary: &ArchiveSummary) -> DbResult<()> {
        pattern_db::queries::create_archive_summary(&self.pool, summary).await
    }

    // ============================================================================
    // Agent Summaries (from coordination)
    // ============================================================================

    /// Get the agent's current summary.
    pub async fn get_summary(&self) -> DbResult<Option<AgentSummary>> {
        pattern_db::queries::get_agent_summary(&self.pool, &self.agent_id).await
    }

    /// Upsert (insert or update) the agent's summary.
    pub async fn upsert_summary(&self, summary: &AgentSummary) -> DbResult<()> {
        pattern_db::queries::upsert_agent_summary(&self.pool, summary).await
    }

    // ============================================================================
    // Activity Logging (from coordination)
    // ============================================================================

    /// Log an activity event.
    pub async fn log_activity(&self, event: ActivityEvent) -> DbResult<()> {
        pattern_db::queries::create_activity_event(&self.pool, &event).await
    }

    /// Get recent activity events for this agent.
    pub async fn recent_activity(&self, limit: usize) -> DbResult<Vec<ActivityEvent>> {
        pattern_db::queries::get_agent_activity(&self.pool, &self.agent_id, limit as i64).await
    }

    // ============================================================================
    // Error Recovery Operations
    // ============================================================================

    /// Clean up a batch by removing unpaired tool calls/responses.
    ///
    /// This is used during error recovery when tool call/response pairing is broken.
    /// It loads all messages in the batch, applies finalize() to remove unpaired
    /// entries, then:
    /// 1. Tombstones the removed messages in the database
    /// 2. Persists any content modifications (e.g., tool calls filtered from blocks)
    ///
    /// Returns the number of messages removed.
    pub async fn cleanup_batch(&self, batch_id: &SnowflakePosition) -> Result<usize, CoreError> {
        // Load all messages in the batch
        let messages = self.get_batch(&batch_id.to_string()).await?;

        if messages.is_empty() {
            return Ok(0);
        }

        // Create a MessageBatch and finalize it to identify unpaired messages
        let batch_type = messages
            .first()
            .and_then(|m| m.batch_type)
            .unwrap_or(crate::message::BatchType::UserRequest);

        let mut batch =
            crate::message::MessageBatch::from_messages(*batch_id, batch_type, messages);
        let removed_ids = batch.finalize();

        // Tombstone the removed messages in the database
        let mut removed_count = 0;
        for msg_id in &removed_ids {
            match pattern_db::queries::delete_message(&self.pool, &msg_id.0).await {
                Ok(_) => {
                    removed_count += 1;
                    tracing::debug!(
                        agent_id = %self.agent_id,
                        message_id = %msg_id.0,
                        batch_id = %batch_id,
                        "Tombstoned unpaired message during batch cleanup"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        message_id = %msg_id.0,
                        error = %e,
                        "Failed to tombstone unpaired message during batch cleanup"
                    );
                }
            }
        }

        // Persist content modifications for remaining messages.
        // finalize() may have modified message content (e.g., filtering tool calls from blocks,
        // replacing content with empty text). We need to persist these changes.
        let mut modified_count = 0;
        for msg in &batch.messages {
            // Serialize the (potentially modified) content
            let content_json = match serde_json::to_value(&msg.content) {
                Ok(v) => sqlx::types::Json(v),
                Err(e) => {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        message_id = %msg.id.0,
                        error = %e,
                        "Failed to serialize message content during batch cleanup"
                    );
                    continue;
                }
            };

            // Extract content preview
            let content_preview = extract_content_preview(&msg.content);

            // Update the message in the database
            match pattern_db::queries::update_message_content(
                &self.pool,
                &msg.id.0,
                &content_json,
                content_preview.as_deref(),
            )
            .await
            {
                Ok(_) => {
                    modified_count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        message_id = %msg.id.0,
                        error = %e,
                        "Failed to update message content during batch cleanup"
                    );
                }
            }
        }

        tracing::info!(
            agent_id = %self.agent_id,
            batch_id = %batch_id,
            removed_count = removed_count,
            modified_count = modified_count,
            "Batch cleanup complete"
        );

        Ok(removed_count)
    }

    /// Force compression of message history by archiving older messages.
    ///
    /// This is used during error recovery when the prompt is too long.
    /// It archives messages beyond a conservative limit to free up context space.
    ///
    /// Returns the number of messages archived.
    pub async fn force_compression(&self, keep_recent: usize) -> Result<usize, CoreError> {
        // Early return if keep_recent is 0 (would archive everything, probably not intended)
        if keep_recent == 0 {
            tracing::warn!(
                agent_id = %self.agent_id,
                "force_compression called with keep_recent=0, refusing to archive all messages"
            );
            return Ok(0);
        }

        // Get message count
        let total_count = self.count().await? as usize;

        if total_count <= keep_recent {
            tracing::debug!(
                agent_id = %self.agent_id,
                total_count = total_count,
                keep_recent = keep_recent,
                "No compression needed - already under limit"
            );
            return Ok(0);
        }

        // Get all non-archived messages to find the cutoff point
        let messages = self.get_recent(total_count).await?;

        if messages.len() <= keep_recent {
            return Ok(0);
        }

        // Messages are ordered newest-first by position (descending order).
        // - Index 0 = newest message (highest/largest position value)
        // - Index n-1 = oldest message (lowest/smallest position value)
        //
        // We want to KEEP the first `keep_recent` messages (indices 0 to keep_recent-1).
        // We want to ARCHIVE everything older (indices keep_recent and beyond).
        //
        // archive_before(P) sets is_archived=1 for all messages where position < P.
        // So we pass the position of the OLDEST message we want to KEEP.
        // All messages with smaller positions (i.e., older messages) get archived.
        //
        // Example: 30 messages, keep_recent = 20
        //   - messages[0..19] = 20 newest (KEEP these)
        //   - messages[20..29] = 10 oldest (ARCHIVE these)
        //   - oldest_keep_index = 20 - 1 = 19
        //   - archive_before(messages[19].position) archives messages[20..29]
        let oldest_keep_index = keep_recent - 1;

        if let Some(cutoff_message) = messages.get(oldest_keep_index) {
            if let Some(ref position) = cutoff_message.position {
                let archived = self.archive_before(&position.to_string()).await?;

                tracing::info!(
                    agent_id = %self.agent_id,
                    total_messages = messages.len(),
                    keep_recent = keep_recent,
                    archived_count = archived,
                    cutoff_position = %position,
                    "Force compression complete"
                );

                return Ok(archived as usize);
            }
        }

        Ok(0)
    }

    /// Add a synthetic user message to ensure non-empty context.
    ///
    /// This is used during error recovery for Gemini empty contents errors.
    /// Gemini requires at least one non-empty message.
    ///
    /// Returns the ID of the created message.
    pub async fn add_synthetic_message(
        &self,
        batch_id: SnowflakePosition,
        content: &str,
    ) -> Result<crate::id::MessageId, CoreError> {
        let message = crate::message::Message::user_in_batch_typed(
            batch_id,
            0, // Will be updated by store logic if needed
            crate::message::BatchType::SystemTrigger,
            content.to_string(),
        );

        self.store(&message).await?;

        tracing::info!(
            agent_id = %self.agent_id,
            batch_id = %batch_id,
            message_id = %message.id.0,
            "Added synthetic message to prevent empty context"
        );

        Ok(message.id)
    }

    // ============================================================================
    // Utilities
    // ============================================================================

    /// Get the agent ID this store is scoped to.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Get a reference to the underlying database pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
