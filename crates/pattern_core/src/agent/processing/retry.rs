//! Model completion with retry logic.
//!
//! Provides robust retry handling for model API calls, including:
//! - Rate limit parsing from multiple header formats
//! - Gemini-specific prompt modifications for empty candidate errors
//! - Exponential backoff with jitter
//! - Error classification for retry decisions

use std::time::Duration;

use rand::Rng;

use crate::ModelProvider;
use crate::messages::{ChatRole, MessageContent, Request, Response};
use crate::model::ResponseOptions;

use super::errors::{ProcessingError, extract_rate_limit_wait_time};

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u8,
    /// Base backoff time in milliseconds
    pub base_backoff_ms: u64,
    /// Maximum backoff time in milliseconds
    pub max_backoff_ms: u64,
    /// Jitter range in milliseconds (added to backoff)
    pub jitter_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            base_backoff_ms: 1000,
            max_backoff_ms: 60_000,
            jitter_ms: 2000,
        }
    }
}

/// Decision about whether to retry after an error.
#[derive(Debug, Clone)]
pub enum RetryDecision {
    /// Retry after waiting, optionally modifying the prompt
    Retry {
        wait_ms: u64,
        modify_prompt: Option<PromptModification>,
    },
    /// Fatal error, don't retry
    Fatal(ProcessingError),
}

/// Modifications to apply to the prompt before retry.
#[derive(Debug, Clone)]
pub enum PromptModification {
    /// Append text to the last user message (Gemini empty candidates fix)
    AppendToLastUserMessage(String),
}

/// Complete a model request with retry logic.
///
/// Handles:
/// - Rate limits (429/529) with backoff from headers
/// - Gemini empty candidates with prompt modifications
/// - Server errors (5xx) with exponential backoff
/// - Authentication errors (fatal, no retry)
pub async fn complete_with_retry(
    model: &dyn ModelProvider,
    response_options: &ResponseOptions,
    request: &mut Request,
    config: &RetryConfig,
) -> Result<Response, ProcessingError> {
    let mut attempts = 0u8;
    let mut gemini_punctuation_idx = 0usize;
    const GEMINI_PUNCTUATION: [&str; 4] = [".", "?", "!", "..."];

    loop {
        attempts += 1;

        match model.complete(response_options, request.clone()).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                let error_str = e.to_string();

                // Check if we've exceeded max attempts
                if attempts >= config.max_attempts {
                    return Err(ProcessingError::ModelCompletion(format!(
                        "Max retries ({}) exceeded. Last error: {}",
                        config.max_attempts, error_str
                    )));
                }

                let decision =
                    classify_error_for_retry(&error_str, attempts, config, gemini_punctuation_idx);

                match decision {
                    RetryDecision::Fatal(err) => return Err(err),
                    RetryDecision::Retry {
                        wait_ms,
                        modify_prompt,
                    } => {
                        tracing::warn!(
                            attempt = attempts,
                            wait_ms,
                            error = %error_str,
                            "Model completion failed, retrying"
                        );

                        if let Some(modification) = modify_prompt {
                            apply_prompt_modification(request, &modification);
                            // Track Gemini punctuation attempts
                            if matches!(
                                modification,
                                PromptModification::AppendToLastUserMessage(_)
                            ) {
                                gemini_punctuation_idx =
                                    (gemini_punctuation_idx + 1) % GEMINI_PUNCTUATION.len();
                            }
                        }

                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    }
                }
            }
        }
    }
}

/// Classify an error to determine retry strategy.
fn classify_error_for_retry(
    error_str: &str,
    attempt: u8,
    config: &RetryConfig,
    gemini_punctuation_idx: usize,
) -> RetryDecision {
    let error_lower = error_str.to_lowercase();
    const GEMINI_PUNCTUATION: [&str; 4] = [".", "?", "!", "..."];

    // Authentication errors are fatal
    if error_lower.contains("401")
        || error_lower.contains("403")
        || error_lower.contains("authentication")
        || error_lower.contains("unauthorized")
        || error_lower.contains("invalid api key")
    {
        return RetryDecision::Fatal(ProcessingError::AuthenticationFailed(error_str.to_string()));
    }

    // Rate limit errors - use wait time from headers/message
    if error_lower.contains("429")
        || error_lower.contains("529")
        || error_lower.contains("rate limit")
        || error_lower.contains("too many requests")
    {
        let wait_seconds = extract_rate_limit_wait_time(error_str);
        let jitter = rand::rng().random_range(0..config.jitter_ms);
        return RetryDecision::Retry {
            wait_ms: (wait_seconds * 1000) + jitter,
            modify_prompt: None,
        };
    }

    // Gemini empty candidates error - try appending punctuation
    if error_lower.contains("empty candidates")
        || error_lower.contains("contents is not specified")
        || (error_lower.contains("gemini") && error_lower.contains("empty"))
    {
        let punctuation = GEMINI_PUNCTUATION[gemini_punctuation_idx % GEMINI_PUNCTUATION.len()];
        return RetryDecision::Retry {
            wait_ms: calculate_backoff(attempt, config),
            modify_prompt: Some(PromptModification::AppendToLastUserMessage(
                punctuation.to_string(),
            )),
        };
    }

    // Context length exceeded - could try compression, but for now treat as recoverable
    if error_lower.contains("context length")
        || error_lower.contains("too long")
        || error_lower.contains("maximum")
            && (error_lower.contains("token") || error_lower.contains("context"))
    {
        return RetryDecision::Fatal(ProcessingError::Recoverable {
            kind: crate::agent::RecoverableErrorKind::PromptTooLong,
            message: error_str.to_string(),
        });
    }

    // Server errors (5xx) - retry with backoff
    if error_lower.contains("500")
        || error_lower.contains("502")
        || error_lower.contains("503")
        || error_lower.contains("504")
        || error_lower.contains("server error")
        || error_lower.contains("internal error")
    {
        return RetryDecision::Retry {
            wait_ms: calculate_backoff(attempt, config),
            modify_prompt: None,
        };
    }

    // Timeout errors - retry with backoff
    if error_lower.contains("timeout") || error_lower.contains("timed out") {
        return RetryDecision::Retry {
            wait_ms: calculate_backoff(attempt, config),
            modify_prompt: None,
        };
    }

    // Default: retry with backoff for unknown errors (up to max_attempts)
    RetryDecision::Retry {
        wait_ms: calculate_backoff(attempt, config),
        modify_prompt: None,
    }
}

/// Calculate exponential backoff with cap.
fn calculate_backoff(attempt: u8, config: &RetryConfig) -> u64 {
    let base = config.base_backoff_ms;
    let exponential = base.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1) as u32));
    let capped = exponential.min(config.max_backoff_ms);
    let jitter = if config.jitter_ms > 0 {
        rand::rng().random_range(0..config.jitter_ms)
    } else {
        0
    };
    capped.saturating_add(jitter)
}

/// Apply a prompt modification to the request.
fn apply_prompt_modification(request: &mut Request, modification: &PromptModification) {
    match modification {
        PromptModification::AppendToLastUserMessage(text) => {
            // Find the last user message and append to it
            for message in request.messages.iter_mut().rev() {
                if matches!(message.role, ChatRole::User) {
                    // Append to the text content
                    if let MessageContent::Text(ref mut t) = message.content {
                        t.push_str(text);
                        tracing::debug!(
                            appended = %text,
                            "Applied Gemini punctuation fix to last user message"
                        );
                        return;
                    }
                }
            }
            tracing::warn!("Could not find user message to apply punctuation fix");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_auth_error() {
        let config = RetryConfig::default();
        let decision = classify_error_for_retry("401 Unauthorized", 1, &config, 0);
        assert!(matches!(decision, RetryDecision::Fatal(_)));

        let decision = classify_error_for_retry("Invalid API key", 1, &config, 0);
        assert!(matches!(decision, RetryDecision::Fatal(_)));
    }

    #[test]
    fn test_classify_rate_limit() {
        let config = RetryConfig::default();
        let decision = classify_error_for_retry("429 Too Many Requests", 1, &config, 0);
        assert!(matches!(
            decision,
            RetryDecision::Retry {
                modify_prompt: None,
                ..
            }
        ));

        let decision = classify_error_for_retry("rate limit exceeded", 1, &config, 0);
        assert!(matches!(
            decision,
            RetryDecision::Retry {
                modify_prompt: None,
                ..
            }
        ));
    }

    #[test]
    fn test_classify_gemini_empty() {
        let config = RetryConfig::default();
        let decision = classify_error_for_retry("empty candidates", 1, &config, 0);
        assert!(matches!(
            decision,
            RetryDecision::Retry {
                modify_prompt: Some(PromptModification::AppendToLastUserMessage(_)),
                ..
            }
        ));
    }

    #[test]
    fn test_classify_server_error() {
        let config = RetryConfig::default();
        let decision = classify_error_for_retry("500 Internal Server Error", 1, &config, 0);
        assert!(matches!(
            decision,
            RetryDecision::Retry {
                modify_prompt: None,
                ..
            }
        ));
    }

    #[test]
    fn test_calculate_backoff() {
        let config = RetryConfig {
            base_backoff_ms: 1000,
            max_backoff_ms: 60_000,
            jitter_ms: 0, // No jitter for deterministic test
            ..Default::default()
        };

        assert_eq!(calculate_backoff(1, &config), 1000);
        assert_eq!(calculate_backoff(2, &config), 2000);
        assert_eq!(calculate_backoff(3, &config), 4000);
        assert_eq!(calculate_backoff(4, &config), 8000);
        // Should cap at max
        assert_eq!(calculate_backoff(10, &config), 60_000);
    }

    #[test]
    fn test_default_config() {
        let config = RetryConfig::default();
        assert_eq!(config.max_attempts, 10);
        assert_eq!(config.base_backoff_ms, 1000);
        assert_eq!(config.max_backoff_ms, 60_000);
        assert_eq!(config.jitter_ms, 2000);
    }
}
