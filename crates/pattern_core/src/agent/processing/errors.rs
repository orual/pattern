//! Error handling for the processing loop.
//!
//! Provides centralized error handling, classification, and recovery logic.

use tokio::sync::mpsc;

use crate::SnowflakePosition;
use crate::agent::{RecoverableErrorKind, ResponseEvent};
use crate::runtime::AgentRuntime;

/// Errors that can occur during message processing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProcessingError {
    /// Failed to build context for model request
    #[error("context build failed: {0}")]
    ContextBuild(String),

    /// Model completion failed
    #[error("model completion failed: {0}")]
    ModelCompletion(String),

    /// Failed to store message
    #[error("message storage failed: {0}")]
    MessageStorage(String),

    /// No model options configured
    #[error("no model options configured: {0}")]
    NoModelOptions(String),

    /// Rate limit exceeded
    #[error("rate limit exceeded: wait {wait_seconds}s")]
    RateLimit { wait_seconds: u64 },

    /// Authentication error (non-recoverable)
    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Generic recoverable error
    #[error("{message}")]
    Recoverable {
        kind: RecoverableErrorKind,
        message: String,
    },
}

impl ProcessingError {
    /// Classify this error into kind, message, and recoverability.
    pub fn classify(&self) -> (RecoverableErrorKind, String, bool) {
        match self {
            Self::ContextBuild(msg) => {
                (RecoverableErrorKind::ContextBuildFailed, msg.clone(), true)
            }
            Self::ModelCompletion(msg) => {
                let kind = RecoverableErrorKind::from_error_str(msg);
                let recoverable = !matches!(kind, RecoverableErrorKind::Unknown);
                (kind, msg.clone(), recoverable)
            }
            Self::MessageStorage(msg) => {
                (RecoverableErrorKind::ContextBuildFailed, msg.clone(), true)
            }
            Self::NoModelOptions(msg) => (RecoverableErrorKind::Unknown, msg.clone(), false),
            Self::RateLimit { wait_seconds } => (
                RecoverableErrorKind::ModelApiError,
                format!("Rate limit exceeded, wait {}s", wait_seconds),
                true,
            ),
            Self::AuthenticationFailed(msg) => (RecoverableErrorKind::Unknown, msg.clone(), false),
            Self::Recoverable { kind, message } => (kind.clone(), message.clone(), true),
        }
    }
}

/// Context needed for error handling.
pub struct ErrorContext<'a> {
    pub event_tx: &'a mpsc::Sender<ResponseEvent>,
    pub runtime: &'a AgentRuntime,
    pub batch_id: Option<SnowflakePosition>,
    pub agent_id: &'a str,
}

/// Handle a processing error: emit event, run recovery, return outcome.
///
/// This centralizes the error handling pattern that was previously repeated
/// multiple times in the processing loop.
pub async fn handle_processing_error(ctx: &ErrorContext<'_>, error: &ProcessingError) {
    let (kind, message, recoverable) = error.classify();

    // Emit error event
    let _ = ctx
        .event_tx
        .send(ResponseEvent::Error {
            message: message.clone(),
            recoverable,
        })
        .await;

    // Run recovery
    run_error_recovery(ctx.runtime, ctx.agent_id, &kind, &message, ctx.batch_id).await;
}

/// Run error recovery based on the error kind.
///
/// This performs cleanup and recovery actions based on the type of error
/// encountered, making the agent more resilient to API quirks and transient issues.
///
/// The recovery actions are based on production experience with Anthropic, Gemini,
/// and other model providers.
pub async fn run_error_recovery(
    runtime: &AgentRuntime,
    agent_id: &str,
    error_kind: &RecoverableErrorKind,
    error_msg: &str,
    batch_id: Option<SnowflakePosition>,
) {
    tracing::warn!(
        agent_id = %agent_id,
        error_kind = ?error_kind,
        batch_id = ?batch_id,
        "Running error recovery: {}",
        error_msg
    );

    match error_kind {
        RecoverableErrorKind::AnthropicThinkingOrder => {
            // Anthropic thinking mode requires specific message ordering.
            // Recovery: Clean up the batch to remove unpaired tool calls.
            tracing::info!(
                agent_id = %agent_id,
                "Anthropic thinking order error - cleaning up batch"
            );

            if let Some(batch) = batch_id {
                match runtime.messages().cleanup_batch(&batch).await {
                    Ok(removed) => {
                        tracing::info!(
                            agent_id = %agent_id,
                            batch = %batch,
                            removed_count = removed,
                            "Cleaned up batch for Anthropic thinking order fix"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            agent_id = %agent_id,
                            batch = %batch,
                            error = %e,
                            "Failed to clean up batch for Anthropic thinking order"
                        );
                    }
                }
            }
        }

        RecoverableErrorKind::GeminiEmptyContents => {
            // Gemini fails when contents array is empty.
            // Recovery: Clean up empty messages, add synthetic if needed.
            tracing::info!(
                agent_id = %agent_id,
                "Gemini empty contents error - cleaning up empty messages"
            );

            if let Some(batch) = batch_id {
                // First, try cleaning up the batch
                match runtime.messages().cleanup_batch(&batch).await {
                    Ok(removed) => {
                        tracing::info!(
                            agent_id = %agent_id,
                            batch = %batch,
                            removed_count = removed,
                            "Cleaned up empty messages for Gemini"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "Failed to cleanup batch, will add synthetic message"
                        );
                    }
                }

                // Check if batch is now empty and add synthetic message if needed
                match runtime.messages().get_batch(&batch.to_string()).await {
                    Ok(messages) => {
                        if messages.is_empty() {
                            match runtime
                                .messages()
                                .add_synthetic_message(batch, "[System: Continuing conversation]")
                                .await
                            {
                                Ok(msg_id) => {
                                    tracing::info!(
                                        agent_id = %agent_id,
                                        batch = %batch,
                                        message_id = %msg_id.0,
                                        "Added synthetic message to prevent empty Gemini context"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        agent_id = %agent_id,
                                        error = %e,
                                        "Failed to add synthetic message for Gemini"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "Failed to check batch contents"
                        );
                    }
                }
            }
        }

        RecoverableErrorKind::UnpairedToolCalls | RecoverableErrorKind::UnpairedToolResponses => {
            // Tool call/response pairs must match.
            // Recovery: Remove unpaired entries from the batch.
            tracing::info!(
                agent_id = %agent_id,
                "Unpaired tool call/response error - cleaning up batch"
            );

            if let Some(batch) = batch_id {
                match runtime.messages().cleanup_batch(&batch).await {
                    Ok(removed) => {
                        tracing::info!(
                            agent_id = %agent_id,
                            batch = %batch,
                            removed_count = removed,
                            "Removed unpaired tool calls/responses from batch"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            agent_id = %agent_id,
                            batch = %batch,
                            error = %e,
                            "Failed to clean up unpaired tool calls/responses"
                        );
                    }
                }
            }
        }

        RecoverableErrorKind::PromptTooLong => {
            // Prompt exceeds token limit.
            // Recovery: Force aggressive compression.
            tracing::info!(
                agent_id = %agent_id,
                "Prompt too long - forcing context compression"
            );

            const EMERGENCY_KEEP_RECENT: usize = 20;

            match runtime
                .messages()
                .force_compression(EMERGENCY_KEEP_RECENT)
                .await
            {
                Ok(archived) => {
                    tracing::info!(
                        agent_id = %agent_id,
                        archived_count = archived,
                        keep_recent = EMERGENCY_KEEP_RECENT,
                        "Force compression complete - archived {} messages",
                        archived
                    );
                }
                Err(e) => {
                    tracing::error!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to force compression"
                    );
                }
            }
        }

        RecoverableErrorKind::MessageCompressionFailed => {
            // Compression itself failed.
            // Recovery: Clean up problematic batches.
            tracing::info!(
                agent_id = %agent_id,
                "Message compression failed - cleaning up current batch"
            );

            if let Some(batch) = batch_id {
                match runtime.messages().cleanup_batch(&batch).await {
                    Ok(removed) => {
                        if removed > 0 {
                            tracing::info!(
                                agent_id = %agent_id,
                                batch = %batch,
                                removed_count = removed,
                                "Cleaned up batch after compression failure"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "Failed to clean up batch after compression failure"
                        );
                    }
                }
            }
        }

        RecoverableErrorKind::ContextBuildFailed => {
            // Context building failed.
            // Recovery: Clean up current batch.
            tracing::info!(
                agent_id = %agent_id,
                "Context build failed - cleaning up for rebuild"
            );

            if let Some(batch) = batch_id {
                match runtime.messages().cleanup_batch(&batch).await {
                    Ok(removed) => {
                        tracing::info!(
                            agent_id = %agent_id,
                            batch = %batch,
                            removed_count = removed,
                            "Cleaned up batch for context rebuild"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "Failed to clean up batch for context rebuild"
                        );
                    }
                }
            }
        }

        RecoverableErrorKind::ModelApiError => {
            // Generic model API error (rate limit, server error, etc.)
            let is_rate_limit = error_msg.contains("429")
                || error_msg.to_lowercase().contains("rate limit")
                || error_msg.to_lowercase().contains("too many requests");

            if is_rate_limit {
                let wait_seconds = extract_rate_limit_wait_time(error_msg);

                tracing::info!(
                    agent_id = %agent_id,
                    wait_seconds = wait_seconds,
                    "Rate limit hit - waiting before retry"
                );

                tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds)).await;

                tracing::info!(
                    agent_id = %agent_id,
                    "Rate limit wait complete, ready for retry"
                );
            } else {
                tracing::info!(
                    agent_id = %agent_id,
                    "Model API error (non-rate-limit) - will retry"
                );
            }
        }

        RecoverableErrorKind::Unknown => {
            // Unknown error type - do generic cleanup.
            tracing::warn!(
                agent_id = %agent_id,
                "Unknown error type - performing generic cleanup"
            );

            if let Some(batch) = batch_id {
                if let Err(e) = runtime.messages().cleanup_batch(&batch).await {
                    tracing::warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed generic batch cleanup"
                    );
                }
            }
        }
    }

    // Prune any expired state from the tool executor
    runtime.prune_expired();

    tracing::info!(
        agent_id = %agent_id,
        "Error recovery complete"
    );
}

/// Extract wait time from rate limit error messages.
///
/// Attempts to parse common rate limit response formats:
/// - "retry-after: 30" header value
/// - "wait 30 seconds" in message
/// - "reset in 30s" in message
///
/// Returns a default backoff if parsing fails.
pub fn extract_rate_limit_wait_time(error_msg: &str) -> u64 {
    let error_lower = error_msg.to_lowercase();

    // Try to find "retry-after: N" or "retry after N"
    if let Some(idx) = error_lower.find("retry") {
        let after_retry = &error_msg[idx..];
        if let Some(num_start) = after_retry.find(|c: char| c.is_ascii_digit()) {
            let num_str: String = after_retry[num_start..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(seconds) = num_str.parse::<u64>() {
                return seconds.min(300); // Cap at 5 minutes
            }
        }
    }

    // Try to find "wait N seconds" or "N seconds"
    if let Some(idx) = error_lower.find("second") {
        let before_seconds = &error_msg[..idx];
        let num_str: String = before_seconds
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == ' ')
            .collect::<String>()
            .chars()
            .rev()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if let Ok(seconds) = num_str.parse::<u64>() {
            return seconds.min(300);
        }
    }

    // Try to find "reset in Ns" pattern
    if let Some(idx) = error_lower.find("reset") {
        let after_reset = &error_msg[idx..];
        if let Some(num_start) = after_reset.find(|c: char| c.is_ascii_digit()) {
            let num_str: String = after_reset[num_start..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(seconds) = num_str.parse::<u64>() {
                return seconds.min(300);
            }
        }
    }

    // Default: exponential backoff starting at 30 seconds
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_retry_after() {
        assert_eq!(extract_rate_limit_wait_time("retry-after: 30"), 30);
        assert_eq!(extract_rate_limit_wait_time("Retry-After: 60"), 60);
        assert_eq!(extract_rate_limit_wait_time("retry after 45 seconds"), 45);
    }

    #[test]
    fn test_extract_seconds() {
        assert_eq!(extract_rate_limit_wait_time("wait 30 seconds"), 30);
        assert_eq!(extract_rate_limit_wait_time("please wait 120 seconds"), 120);
    }

    #[test]
    fn test_extract_reset() {
        assert_eq!(extract_rate_limit_wait_time("reset in 15s"), 15);
        assert_eq!(extract_rate_limit_wait_time("will reset in 45 seconds"), 45);
    }

    #[test]
    fn test_extract_caps_at_300() {
        assert_eq!(extract_rate_limit_wait_time("retry-after: 600"), 300);
        assert_eq!(extract_rate_limit_wait_time("wait 1000 seconds"), 300);
    }

    #[test]
    fn test_extract_default() {
        assert_eq!(extract_rate_limit_wait_time("some random error"), 30);
        assert_eq!(extract_rate_limit_wait_time(""), 30);
    }

    #[test]
    fn test_processing_error_classify() {
        let err = ProcessingError::ContextBuild("test".to_string());
        let (kind, msg, recoverable) = err.classify();
        assert!(matches!(kind, RecoverableErrorKind::ContextBuildFailed));
        assert_eq!(msg, "test");
        assert!(recoverable);

        let err = ProcessingError::AuthenticationFailed("bad key".to_string());
        let (_, _, recoverable) = err.classify();
        assert!(!recoverable);
    }
}
