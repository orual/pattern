//! Message compression strategies for managing context window limits
//!
//! This module implements various strategies for compressing message history
//! when it exceeds the context window, following the MemGPT paper's approach.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    CoreError, ModelProvider, Result,
    messages::{ChatRole, ContentBlock, Message, MessageContent},
};

/// Detect provider from model string
fn detect_provider_from_model(model: &str) -> String {
    let model_lower = model.to_lowercase();

    if model_lower.contains("claude") {
        "anthropic".to_string()
    } else if model_lower.contains("gpt") {
        "openai".to_string()
    } else if model_lower.contains("gemini") {
        "gemini".to_string()
    } else if model_lower.contains("llama") || model_lower.contains("mixtral") {
        "groq".to_string()
    } else if model_lower.contains("command") {
        "cohere".to_string()
    } else if model_lower.contains("deepseek") {
        "deepseek".to_string()
    } else if model_lower.contains("o1") || model_lower.contains("o3") {
        "openai".to_string()
    } else {
        // Default to openai as it's most common
        "openai".to_string()
    }
}

/// Strategy for compressing messages when context is full
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompressionStrategy {
    /// Simple truncation - keep only the most recent messages
    Truncate { keep_recent: usize },

    /// Recursive summarization as described in MemGPT paper
    RecursiveSummarization {
        /// Number of messages to summarize at a time
        chunk_size: usize,
        /// Model to use for summarization
        summarization_model: String,
        /// Optional custom summarization prompt (can include {persona} placeholder)
        #[serde(default)]
        summarization_prompt: Option<String>,
    },

    /// Importance-based selection
    ImportanceBased {
        /// Keep this many recent messages
        keep_recent: usize,
        /// Keep this many important messages
        keep_important: usize,
    },

    /// Time-decay based compression
    TimeDecay {
        /// Messages older than this are candidates for compression
        compress_after_hours: f64,
        /// Keep at least this many recent messages
        min_keep_recent: usize,
    },
}

impl Default for CompressionStrategy {
    fn default() -> Self {
        Self::Truncate { keep_recent: 100 }
    }
}

#[derive(Debug, Clone)]
pub struct CompressionResult {
    /// Batches to keep in the active context
    pub active_batches: Vec<crate::messages::MessageBatch>,

    /// Summary of compressed batches (if applicable)
    pub summary: Option<String>,

    /// Batches moved to recall storage
    pub archived_batches: Vec<crate::messages::MessageBatch>,

    /// Metadata about the compression
    pub metadata: CompressionMetadata,
}

/// Metadata about a compression operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionMetadata {
    pub strategy_used: String,
    pub original_count: usize,
    pub compressed_count: usize,
    pub archived_count: usize,
    pub compression_time: DateTime<Utc>,
    pub estimated_tokens_saved: usize,
}

/// Configuration for importance scoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportanceScoringConfig {
    /// Weight for system messages (default: 10.0)
    pub system_weight: f32,
    /// Weight for assistant messages (default: 3.0)
    pub assistant_weight: f32,
    /// Weight for user messages (default: 5.0)
    pub user_weight: f32,
    /// Weight for other messages (default: 1.0)
    pub other_weight: f32,
    /// Maximum recency bonus (default: 5.0)
    pub recency_bonus: f32,
    /// Weight per 100 characters of content (default: 1.0, max 3.0)
    pub content_length_weight: f32,
    /// Bonus for messages with questions (default: 2.0)
    pub question_bonus: f32,
    /// Bonus for messages with tool calls (default: 4.0)
    pub tool_call_bonus: f32,
    /// Additional keywords to boost importance
    pub important_keywords: Vec<String>,
    /// Bonus per important keyword found (default: 1.5)
    pub keyword_bonus: f32,
}

impl Default for ImportanceScoringConfig {
    fn default() -> Self {
        Self {
            system_weight: 10.0,
            assistant_weight: 3.0,
            user_weight: 5.0,
            other_weight: 1.0,
            recency_bonus: 5.0,
            content_length_weight: 1.0,
            question_bonus: 2.0,
            tool_call_bonus: 4.0,
            important_keywords: vec![
                "important".to_string(),
                "remember".to_string(),
                "critical".to_string(),
                "always".to_string(),
                "never".to_string(),
            ],
            keyword_bonus: 1.5,
        }
    }
}

/// Compresses messages using various strategies
pub struct MessageCompressor {
    strategy: CompressionStrategy,
    model_provider: Option<Arc<dyn ModelProvider>>,
    scoring_config: ImportanceScoringConfig,
    system_prompt_tokens: usize,
    existing_archive_summary: Option<String>,
}

impl MessageCompressor {
    /// Create a new message compressor
    pub fn new(strategy: CompressionStrategy) -> Self {
        Self {
            strategy,
            model_provider: None,
            scoring_config: ImportanceScoringConfig::default(),
            system_prompt_tokens: 0,
            existing_archive_summary: None,
        }
    }

    /// Set the system prompt token count (includes memory blocks)
    pub fn with_system_prompt_tokens(mut self, tokens: usize) -> Self {
        self.system_prompt_tokens = tokens;
        self
    }

    /// Set the existing archive summary to build upon
    pub fn with_existing_summary(mut self, summary: Option<String>) -> Self {
        self.existing_archive_summary = summary;
        self
    }

    /// Set the model provider for strategies that need it
    pub fn with_model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    /// Set custom scoring configuration
    pub fn with_scoring_config(mut self, config: ImportanceScoringConfig) -> Self {
        self.scoring_config = config;
        self
    }

    /// Compress batches according to the configured strategy
    pub async fn compress(
        &self,
        batches: Vec<crate::messages::MessageBatch>,
        max_messages: usize,
        max_tokens: Option<usize>,
    ) -> Result<CompressionResult> {
        // Calculate total message count across all batches
        let original_count: usize = batches.iter().map(|b| b.len()).sum();

        // Calculate total token count if we have a limit
        let original_tokens = if max_tokens.is_some() {
            self.system_prompt_tokens + self.estimate_tokens_from_batches(&batches)
        } else {
            0
        };

        tracing::info!(
            "tokens before compression: {} of max {:?}",
            original_tokens,
            max_tokens
        );

        // Check if we're within both limits
        let within_message_limit = original_count <= max_messages;
        let within_token_limit = max_tokens.map_or(true, |max| original_tokens <= max);

        if within_message_limit && within_token_limit {
            // No compression needed
            return Ok(CompressionResult {
                active_batches: batches,
                summary: None,
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "none".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }

        let result = match &self.strategy {
            CompressionStrategy::Truncate { keep_recent } => {
                self.truncate_messages(batches, *keep_recent, max_messages, max_tokens)
            }

            CompressionStrategy::RecursiveSummarization {
                chunk_size,
                summarization_model,
                summarization_prompt,
            } => {
                self.recursive_summarization(
                    batches,
                    max_messages,
                    max_tokens,
                    *chunk_size,
                    summarization_model,
                    summarization_prompt.as_deref(),
                )
                .await
            }

            CompressionStrategy::ImportanceBased {
                keep_recent,
                keep_important,
            } => {
                self.importance_based_compression(
                    batches,
                    *keep_recent,
                    *keep_important,
                    max_messages,
                    max_tokens,
                )
                .await
            }

            CompressionStrategy::TimeDecay {
                compress_after_hours,
                min_keep_recent,
            } => self.time_decay_compression(
                batches,
                *compress_after_hours,
                *min_keep_recent,
                max_messages,
                max_tokens,
            ),
        }?;

        // Validate and fix message sequence for Gemini compatibility
        //result.active_messages = self.ensure_valid_message_sequence(result.active_messages);

        Ok(result)
    }

    /// Check if a message contains tool use blocks
    fn has_tool_use_blocks(&self, message: &Message) -> bool {
        match &message.content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. })),
            _ => false,
        }
    }

    /// Simple truncation strategy with chunk-based compression
    fn truncate_messages(
        &self,
        batches: Vec<crate::messages::MessageBatch>,
        keep_recent: usize,
        max_messages: usize,
        max_tokens: Option<usize>,
    ) -> Result<CompressionResult> {
        let max_tokens = if let Some(max_tokens) = max_tokens {
            // Account for system prompt when setting the adjusted limit
            Some((max_tokens.saturating_sub(self.system_prompt_tokens)) * 2 / 3)
        } else {
            None
        };

        let original_count: usize = batches.iter().map(|b| b.len()).sum();
        let original_tokens = if max_tokens.is_some() {
            self.estimate_tokens_from_batches(&batches)
        } else {
            0
        };

        // Check if we're within both limits
        let within_message_limit = original_count <= max_messages;
        let within_token_limit = max_tokens.map_or(true, |max| original_tokens <= max);

        // If we're within limits, no compression needed
        if within_message_limit && within_token_limit {
            return Ok(CompressionResult {
                active_batches: batches,
                summary: None,
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "truncate_no_compression".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }

        // We're over limits - keep only keep_recent messages from the most recent batches
        let mut active_batches = Vec::new();
        let mut archived_batches = Vec::new();
        let mut message_count = 0;

        // Iterate from the end (most recent batches) and keep up to keep_recent messages
        for batch in batches.into_iter().rev() {
            if message_count < keep_recent {
                message_count += batch.len();
                active_batches.push(batch);
            } else {
                archived_batches.push(batch);
            }
        }

        // Never archive incomplete batches - keep them active
        let mut incomplete_batches = Vec::new();
        archived_batches.retain(|batch| {
            if !batch.is_complete {
                incomplete_batches.push(batch.clone());
                false
            } else {
                true
            }
        });

        // Add incomplete batches to active
        active_batches.extend(incomplete_batches);

        // Always keep at least one batch (the most recent complete one if possible)
        if active_batches.is_empty() && !archived_batches.is_empty() {
            active_batches.push(archived_batches.pop().unwrap());
        }

        // Reverse to maintain chronological order
        active_batches.reverse();
        archived_batches.reverse();

        let archived_count: usize = archived_batches.iter().map(|b| b.len()).sum();

        // No need to validate tool call/response ordering since batches maintain integrity

        Ok(CompressionResult {
            active_batches,
            summary: None,
            archived_batches: archived_batches.clone(),
            metadata: CompressionMetadata {
                strategy_used: "truncate".to_string(),
                original_count,
                compressed_count: archived_count,
                archived_count,
                compression_time: Utc::now(),
                estimated_tokens_saved: self.estimate_tokens_from_batches(&archived_batches),
            },
        })
    }

    /// Recursive summarization following MemGPT approach
    async fn recursive_summarization(
        &self,
        mut batches: Vec<crate::messages::MessageBatch>,
        max_messages: usize,
        max_tokens: Option<usize>,
        chunk_size: usize,
        summarization_model: &str,
        summarization_prompt: Option<&str>,
    ) -> Result<CompressionResult> {
        if self.model_provider.is_none() {
            return Err(CoreError::ConfigurationError {
                config_path: "compression".to_string(),
                field: "model_provider".to_string(),
                expected: "ModelProvider required for recursive summarization".to_string(),
                cause: crate::error::ConfigError::MissingField("model_provider".to_string()),
            });
        }

        let max_tokens = if let Some(max_tokens) = max_tokens {
            // Account for system prompt when setting the adjusted limit
            Some((max_tokens.saturating_sub(self.system_prompt_tokens)) * 2 / 3)
        } else {
            None
        };

        // Sort batches by batch_id (oldest first)
        batches.sort_by_key(|b| b.id);

        let original_count: usize = batches.iter().map(|b| b.len()).sum();
        let original_tokens = if max_tokens.is_some() {
            self.estimate_tokens_from_batches(&batches)
        } else {
            0
        };

        // Check if we're within both limits
        let within_message_limit = original_count <= max_messages;
        let within_token_limit = max_tokens.map_or(true, |max| original_tokens <= max);

        if within_message_limit && within_token_limit {
            // No compression needed
            return Ok(CompressionResult {
                active_batches: batches,
                summary: self.existing_archive_summary.clone(),
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "recursive_summarization_no_compression".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }

        // Calculate how many messages we need to archive to get under limits
        // We want to archive at least chunk_size messages when over the limit
        let messages_to_archive = if original_count > max_messages {
            // Archive enough to get back under the limit, at minimum chunk_size
            chunk_size.max(original_count - max_messages + chunk_size)
        } else if let Some(max_tok) = max_tokens {
            if original_tokens > max_tok {
                // Over token limit, archive at least chunk_size messages
                chunk_size
            } else {
                0
            }
        } else {
            0
        };

        let mut active_batches = Vec::new();
        let mut archived_batches = Vec::new();
        let mut archived_count = 0;

        // Iterate from oldest to newest, archiving until we have enough
        for mut batch in batches.into_iter() {
            if archived_count < messages_to_archive {
                // Unconditionally archive oldest batches until we have enough
                archived_count += batch.len();
                batch.finalize();
                archived_batches.push(batch);
            } else {
                // Keep remaining batches as active
                active_batches.push(batch);
            }
        }

        // Never archive incomplete batches - keep them active
        let mut incomplete_batches = Vec::new();
        archived_batches.retain(|batch| {
            if !batch.is_complete {
                incomplete_batches.push(batch.clone());
                false
            } else {
                true
            }
        });

        // Add incomplete batches to active
        active_batches.extend(incomplete_batches);

        // Always keep at least one batch (the most recent complete one if possible)
        if active_batches.is_empty() && !archived_batches.is_empty() {
            active_batches.push(archived_batches.pop().unwrap());
        }

        // Restore chronological order (oldest to newest)
        active_batches.reverse();
        archived_batches.reverse();

        if archived_batches.is_empty() {
            // Nothing to summarize
            return Ok(CompressionResult {
                active_batches,
                summary: self.existing_archive_summary.clone(),
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "recursive_summarization".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }

        // Process batches recursively, including previous summaries in each request
        const MAX_TOKENS_PER_REQUEST: usize = 128_000; // Conservative limit for safety

        let mut accumulated_summaries = Vec::new();
        if let Some(ref summary) = self.existing_archive_summary {
            accumulated_summaries.push(super::clip_archive_summary(&summary, 4, 8));
        }

        let mut batch_index = 0;

        while batch_index < archived_batches.len() {
            let mut current_batch_group = Vec::new();
            let mut current_tokens = 0;

            // Calculate tokens for existing summaries + prompt overhead
            let summaries_tokens = self.estimate_summary_tokens(&accumulated_summaries);
            let prompt_overhead = 500; // Rough estimate for system prompt + summarization directive
            let available_tokens =
                MAX_TOKENS_PER_REQUEST.saturating_sub(summaries_tokens + prompt_overhead);

            // Add batches until we would exceed the token limit
            while batch_index < archived_batches.len() {
                let batch = &archived_batches[batch_index];
                let batch_tokens = self.estimate_tokens_from_batches(&[batch.clone()]);

                if current_tokens + batch_tokens > available_tokens
                    && !current_batch_group.is_empty()
                {
                    break; // Would exceed limit, process what we have
                }
                let mut batch = batch.clone();
                batch.finalize();

                current_batch_group.push(batch);
                current_tokens += batch_tokens;
                batch_index += 1;
            }

            // Flatten current batch group to messages
            let group_messages: Vec<Message> = current_batch_group
                .iter()
                .flat_map(|b| b.messages.clone())
                .collect();

            // Generate summary including all previous summaries
            if let Some(group_summary) = self
                .generate_recursive_summary(
                    &accumulated_summaries,
                    &group_messages,
                    summarization_model,
                    summarization_prompt,
                )
                .await
            {
                accumulated_summaries.push(group_summary);
            } else {
                tracing::warn!("Failed to generate summary for batch group, skipping");
            }
        }

        // The final summary is the last (most comprehensive) summary
        let final_summary = accumulated_summaries.into_iter().last();

        // No need to validate tool ordering - batches maintain integrity
        let archived_count: usize = archived_batches.iter().map(|b| b.len()).sum();
        let estimated_tokens_saved = self.estimate_tokens_from_batches(&archived_batches);

        Ok(CompressionResult {
            active_batches,
            summary: final_summary,
            archived_batches,
            metadata: CompressionMetadata {
                strategy_used: "recursive_summarization".to_string(),
                original_count,
                compressed_count: archived_count,
                archived_count,
                compression_time: Utc::now(),
                estimated_tokens_saved,
            },
        })
    }

    /// Importance-based compression using heuristics or LLM
    async fn importance_based_compression(
        &self,
        mut batches: Vec<crate::messages::MessageBatch>,
        keep_recent: usize,
        keep_important: usize,
        max_messages: usize,
        max_tokens: Option<usize>,
    ) -> Result<CompressionResult> {
        // Sort batches by batch_id (oldest first)
        batches.sort_by_key(|b| b.id);

        let original_count: usize = batches.iter().map(|b| b.len()).sum();
        let original_tokens = if max_tokens.is_some() {
            self.estimate_tokens_from_batches(&batches)
        } else {
            0
        };

        let max_tokens = if let Some(max_tokens) = max_tokens {
            // Account for system prompt when setting the adjusted limit
            Some((max_tokens.saturating_sub(self.system_prompt_tokens)) * 2 / 3)
        } else {
            None
        };

        // Check if we're within both limits
        let within_message_limit = original_count <= max_messages;
        let within_token_limit = max_tokens.map_or(true, |max| original_tokens <= max);

        if within_message_limit && within_token_limit {
            // No compression needed
            return Ok(CompressionResult {
                active_batches: batches,
                summary: None,
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "importance_based_no_compression".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }

        // For importance scoring, we need to work with individual messages
        // But we'll try to keep batches intact where possible

        // First, separate recent batches we always keep
        let mut active_batches = Vec::new();
        let mut older_batches = Vec::new();
        let mut recent_message_count = 0;

        // Keep recent batches (from newest)
        for batch in batches.into_iter().rev() {
            if recent_message_count < keep_recent {
                recent_message_count += batch.len();
                active_batches.push(batch);
            } else {
                older_batches.push(batch);
            }
        }

        // Restore chronological order
        active_batches.reverse();
        older_batches.reverse();

        if older_batches.is_empty() {
            // Nothing to compress
            return Ok(CompressionResult {
                active_batches,
                summary: None,
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "importance_based".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        };

        // Score older batches based on their messages
        let mut scored_batches: Vec<(f32, crate::messages::MessageBatch)> = Vec::new();

        for batch in older_batches.iter() {
            // Calculate batch score as average of message scores
            let mut total_score = 0.0;
            let mut count = 0;

            for (idx, msg) in batch.messages.iter().enumerate() {
                let score = if self.model_provider.is_some() {
                    // Use LLM to score importance
                    self.score_message_with_llm(msg).await.unwrap_or_else(|_| {
                        // Fall back to heuristic if LLM fails
                        self.score_message_heuristic(msg, idx, batch.messages.len())
                    })
                } else {
                    // Use heuristic scoring
                    self.score_message_heuristic(msg, idx, batch.messages.len())
                };

                total_score += score;
                count += 1;
            }

            let batch_score = if count > 0 {
                total_score / count as f32
            } else {
                0.0
            };
            scored_batches.push((batch_score, batch.clone()));
        }

        // Sort by score (highest first)
        scored_batches.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Keep the most important batches up to keep_important message count
        let mut important_message_count = 0;
        let mut important_batches = Vec::new();
        let mut archived_batches = Vec::new();

        for (_, batch) in scored_batches {
            if important_message_count < keep_important {
                important_message_count += batch.len();
                important_batches.push(batch);
            } else {
                archived_batches.push(batch);
            }
        }

        // Sort important batches back to chronological order
        important_batches.sort_by_key(|b| b.id);

        // Never archive incomplete batches - keep them active
        let mut incomplete_batches = Vec::new();
        archived_batches.retain(|batch| {
            if !batch.is_complete {
                incomplete_batches.push(batch.clone());
                false
            } else {
                true
            }
        });

        // Add incomplete batches to active
        important_batches.extend(incomplete_batches);

        // Always keep at least one batch (the most recent complete one if possible)
        if important_batches.is_empty() && active_batches.is_empty() && !archived_batches.is_empty()
        {
            important_batches.push(archived_batches.pop().unwrap());
        }

        // Combine important and recent batches
        important_batches.extend(active_batches);
        let active_batches = important_batches;

        // No need to validate tool ordering - batches maintain integrity
        let archived_count: usize = archived_batches.iter().map(|b| b.len()).sum();
        let tokens_saved = self.estimate_tokens_from_batches(&archived_batches);

        Ok(CompressionResult {
            active_batches,
            summary: None,
            archived_batches,
            metadata: CompressionMetadata {
                strategy_used: "importance_based".to_string(),
                original_count,
                compressed_count: archived_count,
                archived_count,
                compression_time: Utc::now(),
                estimated_tokens_saved: tokens_saved,
            },
        })
    }

    /// Score a message's importance using heuristics
    fn score_message_heuristic(&self, msg: &Message, idx: usize, total: usize) -> f32 {
        let mut score = 0.0;

        // Base score by role
        score += match msg.role {
            ChatRole::System => self.scoring_config.system_weight,
            ChatRole::Assistant => self.scoring_config.assistant_weight,
            ChatRole::User => self.scoring_config.user_weight,
            _ => self.scoring_config.other_weight,
        };

        // Recency bonus (newer messages are more important)
        let recency_factor = idx as f32 / total as f32;
        score += recency_factor * self.scoring_config.recency_bonus;

        // Content length bonus (longer messages might contain more information)
        if let Some(text) = msg.text_content() {
            let length_factor = (text.len() as f32 / 100.0).min(3.0);
            score += length_factor * self.scoring_config.content_length_weight;

            // Check for questions
            if text.contains('?') {
                score += self.scoring_config.question_bonus;
            }

            // Check for important keywords
            let text_lower = text.to_lowercase();
            for keyword in &self.scoring_config.important_keywords {
                if text_lower.contains(keyword) {
                    score += self.scoring_config.keyword_bonus;
                }
            }
        }

        // Tool call bonus
        if msg.tool_call_count() > 0 || self.has_tool_use_blocks(msg) {
            score += self.scoring_config.tool_call_bonus;
        }

        score
    }

    /// Score a message's importance using LLM
    async fn score_message_with_llm(&self, msg: &Message) -> Result<f32> {
        if let Some(provider) = &self.model_provider {
            let prompt = format!(
                "Rate the importance of this message in a conversation on a scale of 0-10. \
                 Consider factors like: information content, decisions made, questions asked, \
                 context establishment, and future relevance.\n\n\
                 Message role: {:?}\n\
                 Message content: {}\n\n\
                 Respond with just a number between 0 and 10.",
                msg.role,
                msg.text_content().unwrap_or_default()
            );

            let request = crate::messages::Request {
                system: Some(vec![
                    "You are an expert at evaluating message importance.".to_string(),
                ]),
                messages: vec![Message::user(prompt)],
                tools: None,
            };

            let model_info = crate::model::ModelInfo {
                id: "gpt-3.5-turbo".to_string(),
                name: "gpt-3.5-turbo".to_string(),
                provider: "openai".to_string(),
                capabilities: vec![],
                context_window: 16385,
                max_output_tokens: Some(4096),
                cost_per_1k_prompt_tokens: None,
                cost_per_1k_completion_tokens: None,
            };

            let mut options = crate::model::ResponseOptions::new(model_info);
            options.max_tokens = Some(10);
            options.temperature = Some(0.3);

            match provider.complete(&options, request).await {
                Ok(response) => {
                    if let Ok(score) = response.only_text().trim().parse::<f32>() {
                        return Ok(score.clamp(0.0, 10.0));
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to score message with LLM: {}", e);
                }
            }
        }

        // Fall back to heuristic
        Ok(self.score_message_heuristic(msg, 0, 1))
    }

    /// Time-decay based compression
    fn time_decay_compression(
        &self,
        mut batches: Vec<crate::messages::MessageBatch>,
        compress_after_hours: f64,
        min_keep_recent: usize,
        max_messages: usize,
        max_tokens: Option<usize>,
    ) -> Result<CompressionResult> {
        // Sort batches by batch_id (oldest first)
        batches.sort_by_key(|b| b.id);

        let original_count: usize = batches.iter().map(|b| b.len()).sum();
        let original_tokens = if max_tokens.is_some() {
            self.estimate_tokens_from_batches(&batches)
        } else {
            0
        };

        let max_tokens = if let Some(max_tokens) = max_tokens {
            // Account for system prompt when setting the adjusted limit
            Some((max_tokens.saturating_sub(self.system_prompt_tokens)) * 2 / 3)
        } else {
            None
        };

        // Check if we're within both limits
        let within_message_limit = original_count <= max_messages;
        let within_token_limit = max_tokens.map_or(true, |max| original_tokens <= max);

        if within_message_limit && within_token_limit {
            // No compression needed
            return Ok(CompressionResult {
                active_batches: batches,
                summary: None,
                archived_batches: Vec::new(),
                metadata: CompressionMetadata {
                    strategy_used: "time_decay_no_compression".to_string(),
                    original_count,
                    compressed_count: 0,
                    archived_count: 0,
                    compression_time: Utc::now(),
                    estimated_tokens_saved: 0,
                },
            });
        }
        let now = Utc::now();
        let cutoff_time =
            now - chrono::Duration::milliseconds((compress_after_hours * 3600.0 * 1000.0) as i64);

        // Keep recent batches and batches created after cutoff
        let mut active_batches = Vec::new();
        let mut archived_batches = Vec::new();
        let mut recent_message_count = 0;

        // First pass: keep minimum recent batches (from newest)
        for batch in batches.iter().rev() {
            if recent_message_count < min_keep_recent {
                recent_message_count += batch.len();
                active_batches.push(batch.clone());
            }
        }

        // Second pass: check time cutoff for older batches
        for batch in batches.iter() {
            // Skip if already in active
            if active_batches.iter().any(|b| b.id == batch.id) {
                continue;
            }

            // Check if batch is recent enough (using first message's timestamp as proxy)
            let is_recent = batch
                .messages
                .first()
                .map(|msg| msg.created_at > cutoff_time)
                .unwrap_or(false);

            if is_recent {
                active_batches.push(batch.clone());
            } else if batch.is_complete {
                archived_batches.push(batch.clone());
            } else {
                // Keep incomplete batches active
                active_batches.push(batch.clone());
            }
        }

        // Always keep at least one batch (the most recent one) if we have none
        if active_batches.is_empty() && !archived_batches.is_empty() {
            active_batches.push(archived_batches.pop().unwrap());
        }

        // Sort to maintain chronological order
        active_batches.sort_by_key(|b| b.id);
        archived_batches.sort_by_key(|b| b.id);

        let archived_count: usize = archived_batches.iter().map(|b| b.len()).sum();

        Ok(CompressionResult {
            active_batches,
            summary: None,
            archived_batches: archived_batches.clone(),
            metadata: CompressionMetadata {
                strategy_used: "time_decay".to_string(),
                original_count,
                compressed_count: archived_count,
                archived_count,
                compression_time: now,
                estimated_tokens_saved: self.estimate_tokens_from_batches(&archived_batches),
            },
        })
    }

    /// Estimate tokens for batches
    fn estimate_tokens_from_batches(&self, batches: &[crate::messages::MessageBatch]) -> usize {
        batches
            .iter()
            .flat_map(|b| &b.messages)
            .map(|m| m.estimate_tokens())
            .sum()
    }

    /// Estimate tokens for accumulated summaries
    fn estimate_summary_tokens(&self, summaries: &[String]) -> usize {
        summaries.iter().map(|s| s.len() / 5).sum() // Rough estimate: 4 chars per token
    }

    /// Generate summary including previous summaries for recursive approach
    async fn generate_recursive_summary(
        &self,
        previous_summaries: &[String],
        new_messages: &[Message],
        summarization_model: &str,
        summarization_prompt: Option<&str>,
    ) -> Option<String> {
        if let Some(provider) = &self.model_provider {
            let mut messages_for_summary = Vec::new();

            // Add previous summaries as context if present
            if !previous_summaries.is_empty() {
                let combined_previous = previous_summaries.join("\n\n---Previous Summary---\n\n");
                messages_for_summary.push(Message::system(format!(
                    "Previous summary of conversation:\n{}",
                    combined_previous
                )));
            }

            // Add the actual messages to summarize
            messages_for_summary.extend(new_messages.iter().cloned());

            // Add the summarization directive
            messages_for_summary.push(Message::user(
                "Please summarize all the previous messages, focusing on key information, \
                 decisions made, and important context.

                 preserve: novel insights, unique terminology we've developed, relationship evolution patterns, crisis response validations, architectural discoveries

                 condense: repetitive status updates, routine sync confirmations, similar conversations that don't add new dimensions

                 prioritize: things that would affect future interactions - social calibration lessons learned, boundary discoveries, successful collaboration patterns, failure modes identified

                 remove: duplicate information, overly detailed play-by-plays of routine events

                 If there was a previous summary provided, build upon it, but don't simply extend it.
                 Maintain the conversational style and preserve important details. Keep it as short as reasonable.",
            ));

            let system_prompt = if let Some(custom_prompt) = summarization_prompt {
                vec![custom_prompt.to_string()]
            } else {
                vec![
                    "You are a helpful assistant that creates concise summaries of conversations."
                        .to_string(),
                ]
            };

            let request = crate::messages::Request {
                system: Some(system_prompt),
                messages: messages_for_summary,
                tools: None,
            };

            // Detect provider and create options
            let provider_name = detect_provider_from_model(summarization_model);
            let model_info = crate::model::ModelInfo {
                id: summarization_model.to_string(),
                name: summarization_model.to_string(),
                provider: provider_name,
                capabilities: vec![],
                context_window: 128000,
                max_output_tokens: Some(8192),
                cost_per_1k_prompt_tokens: None,
                cost_per_1k_completion_tokens: None,
            };

            let model_info = crate::model::defaults::enhance_model_info(model_info);
            let mut options = crate::model::ResponseOptions::new(model_info);
            options.max_tokens = Some(8192);
            options.temperature = Some(0.5);

            match provider.complete(&options, request).await {
                Ok(response) => {
                    let summary = response.only_text();
                    tracing::debug!(
                        "Generated summary ({} chars): {:.200}...",
                        summary.len(),
                        &summary
                    );
                    Some(summary)
                }
                Err(e) => {
                    tracing::warn!("Failed to generate summary: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::MessageContent;

    #[test]
    fn test_truncation_strategy() {
        let compressor = MessageCompressor::new(CompressionStrategy::Truncate { keep_recent: 5 });

        let messages = vec![
            Message::user("Hello"),
            Message::agent("Hi there!"),
            Message::user("How are you?"),
            Message::agent("I'm doing well, thanks!"),
            Message::user("What's the weather?"),
            Message::agent("Let me check that for you"),
            Message::user("Any updates?"),
            Message::agent("Still checking..."),
            Message::user("Thanks for checking"),
            Message::agent("You're welcome!"),
        ];

        // Create batches from messages (simple approach: each user-assistant pair is a batch)
        let mut batches = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            // Add small delay to prevent snowflake exhaustion in tests
            std::thread::sleep(std::time::Duration::from_millis(1));
            let batch_id = crate::utils::get_next_message_position_sync();
            let mut batch_messages = vec![messages[i].clone()];
            i += 1;
            // Add assistant response if available
            if i < messages.len() && messages[i].role == ChatRole::Assistant {
                batch_messages.push(messages[i].clone());
                i += 1;
            }
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                batch_messages,
            ));
        }

        let result = tokio_test::block_on(compressor.compress(batches, 5, None)).unwrap();

        // Result now has active_batches not active_messages
        let active_message_count: usize = result.active_batches.iter().map(|b| b.len()).sum();
        let archived_message_count: usize = result.archived_batches.iter().map(|b| b.len()).sum();

        // With batch structure, we keep recent batches (each with 2 messages)
        // So we expect 6 messages (3 batches * 2 messages each)
        assert_eq!(active_message_count, 6);
        // We should have archived some messages
        assert!(archived_message_count > 0);
    }

    #[test]
    fn test_compression_with_tool_calls() {
        let compressor = MessageCompressor::new(CompressionStrategy::Truncate { keep_recent: 5 });

        let mut messages = vec![];

        // Add some conversation before the tool calls
        for i in 0..6 {
            messages.push(Message::user(format!("Question {}", i)));
            messages.push(Message::agent(format!("Answer {}", i)));
            // Small delay to prevent snowflake exhaustion
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // Add tool call sequence
        messages.push(Message::user("Search for something"));
        messages.push(Message {
            role: ChatRole::Assistant,
            content: MessageContent::ToolCalls(vec![crate::messages::ToolCall {
                call_id: "456".to_string(),
                fn_name: "search".to_string(),
                fn_arguments: serde_json::json!({"query": "test"}),
            }]),
            has_tool_calls: true,
            ..Message::agent("test")
        });
        messages.push(Message {
            role: ChatRole::Tool,
            content: MessageContent::ToolResponses(vec![crate::messages::ToolResponse {
                call_id: "456".to_string(),
                content: "Search results".to_string(),
                is_error: Some(false),
            }]),
            ..Message::default()
        });

        // Create batches from messages
        let mut batches = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            let batch_id = crate::utils::get_next_message_position_sync();
            let mut batch_messages = vec![messages[i].clone()];
            i += 1;
            // Add responses until we hit another user message
            while i < messages.len() && messages[i].role != ChatRole::User {
                batch_messages.push(messages[i].clone());
                i += 1;
            }
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                batch_messages,
            ));
        }

        let result = tokio_test::block_on(compressor.compress(batches, 5, None)).unwrap();

        // Count messages in active batches
        let active_message_count: usize = result.active_batches.iter().map(|b| b.len()).sum();
        let archived_message_count: usize = result.archived_batches.iter().map(|b| b.len()).sum();

        // Should keep approximately 5 messages
        assert!(active_message_count >= 5);
        assert!(archived_message_count > 0);

        // The tool call and response should be in the active batches
        let has_tool_call = result
            .active_batches
            .iter()
            .flat_map(|b| &b.messages)
            .any(|m| m.tool_call_count() > 0);
        let has_tool_response = result
            .active_batches
            .iter()
            .flat_map(|b| &b.messages)
            .any(|m| m.role == ChatRole::Tool);

        assert!(has_tool_call);
        assert!(has_tool_response);
    }

    #[test]
    fn test_importance_scoring() {
        let compressor = MessageCompressor::new(CompressionStrategy::ImportanceBased {
            keep_recent: 2,
            keep_important: 1,
        });

        // Small delay to prevent snowflake exhaustion
        std::thread::sleep(std::time::Duration::from_millis(1));
        let msg = Message::user("This is very important: remember my name is Alice");
        let score = compressor.score_message_heuristic(&msg, 0, 10);

        // Should have high score due to "important" keyword and user role
        assert!(score > 5.0);
    }

    #[test]
    fn test_time_decay_compression() {
        let compressor = MessageCompressor::new(CompressionStrategy::TimeDecay {
            compress_after_hours: 1.0,
            min_keep_recent: 10,
        });

        let now = Utc::now();
        let mut messages = Vec::new();

        // Add 20 old messages (2+ hours old)
        for i in 0..20 {
            messages.push(Message {
                created_at: now - chrono::Duration::hours(3) - chrono::Duration::minutes(i as i64),
                ..if i % 2 == 0 {
                    Message::user(format!("Old message {}", i))
                } else {
                    Message::agent(format!("Old response {}", i))
                }
            });
        }

        // Add 5 messages from 30 mins ago (within the 1 hour cutoff)
        for i in 0..5 {
            messages.push(Message {
                created_at: now - chrono::Duration::minutes(30 - i as i64),
                ..if i % 2 == 0 {
                    Message::user(format!("Recent message {}", i))
                } else {
                    Message::agent(format!("Recent response {}", i))
                }
            });
        }

        // Add 5 very recent messages
        for i in 0..5 {
            messages.push(Message {
                created_at: now - chrono::Duration::seconds(60 - i as i64 * 10),
                ..if i % 2 == 0 {
                    Message::user(format!("Very recent message {}", i))
                } else {
                    Message::agent(format!("Very recent response {}", i))
                }
            });
        }

        // Create batches from messages
        let mut batches = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            let batch_id = crate::utils::get_next_message_position_sync();
            let mut batch_messages = vec![messages[i].clone()];
            i += 1;
            // Add responses until we hit another user message
            while i < messages.len() && messages[i].role != ChatRole::User {
                batch_messages.push(messages[i].clone());
                i += 1;
            }
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                batch_messages,
            ));
        }

        let result = tokio_test::block_on(compressor.compress(batches, 15, None)).unwrap();

        // Should keep at least 10 recent messages (min_keep_recent)
        // Plus the 10 messages that are within the 1 hour cutoff
        // Count messages in batches
        let active_message_count: usize = result.active_batches.iter().map(|b| b.len()).sum();
        let archived_message_count: usize = result.archived_batches.iter().map(|b| b.len()).sum();

        assert!(active_message_count >= 10);
        assert!(archived_message_count > 0);
        assert!(
            result
                .archived_batches
                .iter()
                .flat_map(|b| &b.messages)
                .any(|m| m.text_content().unwrap_or_default().contains("Old message"))
        );
    }

    #[test]
    fn test_compression_metadata() {
        let compressor = MessageCompressor::new(CompressionStrategy::Truncate { keep_recent: 1 });

        // Build three batches; ensure first two are complete so they can be archived
        let mut batches = Vec::new();
        // Batch 1: user then assistant (complete)
        {
            let batch_id = crate::utils::get_next_message_position_sync();
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                vec![Message::user("Message 1"), Message::agent("Ack 1")],
            ));
        }
        // Batch 2: assistant only (complete)
        {
            let batch_id = crate::utils::get_next_message_position_sync();
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                vec![Message::agent("Message 2")],
            ));
        }
        // Batch 3: user then assistant (complete and most recent; should be kept)
        {
            let batch_id = crate::utils::get_next_message_position_sync();
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                vec![Message::user("Message 3"), Message::agent("Ack 3")],
            ));
        }

        let result = tokio_test::block_on(compressor.compress(batches, 1, None)).unwrap();

        // With 3 batches constructed as [2,1,2] messages, keep_recent=1 keeps the last (2 msgs).
        // Archived should contain the first two batches (2 + 1 = 3 messages).
        assert_eq!(result.metadata.original_count, 5);
        assert_eq!(result.metadata.compressed_count, 3);
        assert_eq!(result.metadata.archived_count, 3);
        assert_eq!(result.metadata.strategy_used, "truncate");
    }

    #[test]
    fn test_importance_scoring_config_serialization() {
        let config = ImportanceScoringConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ImportanceScoringConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.system_weight, deserialized.system_weight);
        assert_eq!(config.important_keywords, deserialized.important_keywords);
    }

    #[test]
    fn test_compression_strategy_serialization() {
        let strategies = vec![
            CompressionStrategy::Truncate { keep_recent: 50 },
            CompressionStrategy::ImportanceBased {
                keep_recent: 20,
                keep_important: 10,
            },
            CompressionStrategy::TimeDecay {
                compress_after_hours: 24.0,
                min_keep_recent: 10,
            },
        ];

        for strategy in strategies {
            let json = serde_json::to_string(&strategy).unwrap();
            let deserialized: CompressionStrategy = serde_json::from_str(&json).unwrap();

            // Verify roundtrip works
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn test_custom_scoring_config() {
        let mut config = ImportanceScoringConfig::default();
        config.important_keywords.push("deadline".to_string());
        config.question_bonus = 5.0;

        let compressor = MessageCompressor::new(CompressionStrategy::ImportanceBased {
            keep_recent: 1,
            keep_important: 1,
        })
        .with_scoring_config(config);

        // Small delay to prevent snowflake exhaustion
        std::thread::sleep(std::time::Duration::from_millis(1));
        let msg = Message::user("What's the deadline for this project?");
        let score = compressor.score_message_heuristic(&msg, 0, 1);

        // Should have high score due to question and "deadline" keyword
        assert!(score > 10.0);
    }

    #[tokio::test]
    async fn test_importance_based_compression_with_heuristics() {
        let compressor = MessageCompressor::new(CompressionStrategy::ImportanceBased {
            keep_recent: 2,
            keep_important: 2,
        });

        // Small delay to prevent snowflake exhaustion
        tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;

        let messages = vec![
            Message::system("You are a helpful assistant"), // High importance
            Message::user("Hi"),
            Message::agent("Hello!"),
            Message::user("What's very important: my password is 12345"), // High importance
            Message::agent("I understand"),
            Message::user("What's the weather?"),
            Message::agent("Let me check that for you"),
        ];

        // Create batches from messages
        let mut batches = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            let batch_id = crate::utils::get_next_message_position_sync();
            let mut batch_messages = vec![messages[i].clone()];
            i += 1;
            // Add responses until we hit another user message or system message
            while i < messages.len()
                && messages[i].role != ChatRole::User
                && messages[i].role != ChatRole::System
            {
                batch_messages.push(messages[i].clone());
                i += 1;
            }
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                batch_messages,
            ));
        }

        let result = compressor.compress(batches, 4, None).await.unwrap();

        // Count messages
        let active_message_count: usize = result.active_batches.iter().map(|b| b.len()).sum();
        assert!(active_message_count >= 4);

        // System message and important user message should be kept
        let all_active_messages: Vec<_> = result
            .active_batches
            .iter()
            .flat_map(|b| &b.messages)
            .collect();
        assert!(
            all_active_messages
                .iter()
                .any(|m| m.role == ChatRole::System)
        );
        assert!(
            all_active_messages
                .iter()
                .any(|m| m.text_content().unwrap_or_default().contains("password"))
        );
    }

    #[tokio::test]
    async fn test_recursive_summarization() {
        // This test would require a mock ModelProvider
        // For now, we just test the error case
        let compressor = MessageCompressor::new(CompressionStrategy::RecursiveSummarization {
            chunk_size: 5,
            summarization_model: "gpt-3.5-turbo".to_string(),
            summarization_prompt: None,
        });

        let messages = vec![
            Message::user("Message 1"),
            Message::agent("Response 1"),
            Message::user("Message 2"),
            Message::agent("Response 2"),
            Message::user("Message 3"),
        ];

        // Create batches from messages
        let mut batches = Vec::new();
        for msg in messages {
            let batch_id = crate::utils::get_next_message_position_sync();
            batches.push(crate::messages::MessageBatch::from_messages(
                batch_id,
                crate::messages::BatchType::UserRequest,
                vec![msg],
            ));
        }

        let result = compressor.compress(batches, 2, None).await;

        // Should fail without model provider
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_importance_based_compression_with_llm() {
        // Would require mock ModelProvider
        // Testing the fallback behavior
        let compressor = MessageCompressor::new(CompressionStrategy::ImportanceBased {
            keep_recent: 1,
            keep_important: 1,
        });

        let messages = vec![
            Message::user("Remember this important fact"),
            Message::agent("Noted"),
            Message::user("What's 2+2?"),
        ];

        // Create batches from messages
        // Small delay to prevent snowflake exhaustion
        std::thread::sleep(std::time::Duration::from_millis(1));
        let batch_id = crate::utils::get_next_message_position_sync();
        let batch = crate::messages::MessageBatch::from_messages(
            batch_id,
            crate::messages::BatchType::UserRequest,
            messages,
        );
        let batches = vec![batch];

        let result = compressor.compress(batches, 2, None).await.unwrap();

        // Count active messages
        let active_message_count: usize = result.active_batches.iter().map(|b| b.len()).sum();
        // Importance-based keeps the full batch together when important messages are present
        assert_eq!(active_message_count, 3);
    }
}
