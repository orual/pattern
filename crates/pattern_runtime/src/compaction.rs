//! Compaction driver — context-window compression wired into `drive_step`.
//!
//! [`maybe_compact`] is the single entry point called before each wire turn's
//! compose step. It checks the persona's [`ContextPolicy`] gate (message floor
//! and token threshold via async `count_tokens`), dispatches the configured
//! [`CompressionStrategy`], updates `pattern_db` (archive markers and
//! optional summary row), and rewrites the in-memory [`TurnHistory`].
//!
//! # Gate logic
//!
//! Short-circuits in order:
//! 1. Compression disabled (`context_policy.compression` is `None`).
//! 2. Active turn count below `compress_check_message_floor` (default 100).
//! 3. Provider-reported token count below `compress_token_threshold`.
//!
//! Gate (3) calls `ProviderClient::count_tokens` — the only async provider
//! call in the compaction path (excluding RecursiveSummarization's
//! summarizer call).
//!
//! # Strategy dispatch
//!
//! - **Truncate** / **ImportanceBased** / **TimeDecay**: no provider call
//!   beyond the gate; no summary row written.
//! - **RecursiveSummarization**: calls `ProviderClient::complete` to
//!   generate a summary of the oldest chunk, writes a depth-0
//!   `archive_summaries` row, reloads `summary_head`.
//!
//! # Post-strategy invariants
//!
//! - `archive_messages` marks `is_archived=1` for messages with
//!   `position < boundary`.
//! - `TurnHistory::take_oldest` drops archived turns from the active
//!   deque and sets `post_compaction_pending = true` so the next batch
//!   emits a Full snapshot.
//! - `summary_head` is refreshed from `get_summary_head` after any
//!   summary insertion.
//!
//! [`ContextPolicy`]: pattern_core::types::snapshot::ContextPolicy
//! [`CompressionStrategy`]: pattern_core::types::compression::CompressionStrategy
//! [`TurnHistory`]: crate::memory::TurnHistory

use std::sync::Arc;

use futures::StreamExt;
use jiff::Timestamp;

use pattern_core::error::RuntimeError;
use pattern_core::types::compression::CompressionStrategy;
use pattern_core::types::ids::new_snowflake_id;
use pattern_core::types::snapshot::ContextPolicy;

use pattern_provider::compose::compression::{
    DEFAULT_SUMMARIZATION_DIRECTIVE, DEFAULT_SUMMARIZATION_SYSTEM_PROMPT, ImportanceScoringConfig,
    TurnSlice, apply_importance_based, apply_recursive_summarization, apply_time_decay,
    apply_truncate, should_compress,
};

use crate::memory::TurnHistory;
use crate::session::SessionContext;

/// Outcome reported by [`maybe_compact`]. Callers can log or inspect.
#[derive(Debug)]
pub enum CompactionOutcome {
    /// Gate did not fire — compression not needed (below message floor
    /// OR below token threshold OR disabled by persona).
    Skipped {
        /// Human-readable reason the gate did not fire.
        reason: &'static str,
        /// Number of active turns at check time.
        active_turns: usize,
        /// Estimated tokens at check time.
        estimated_tokens: u64,
    },
    /// Gate fired, strategy applied, DB updated, TurnHistory rewritten.
    Fired {
        /// Name of the strategy that ran.
        strategy_name: &'static str,
        /// Number of turns moved to archival.
        archived_turn_count: usize,
        /// Whether a summary row was written to `archive_summaries`.
        summary_written: bool,
        /// Number of active turns remaining after compaction.
        active_after: usize,
    },
}

/// Check the compression gate; apply the persona's strategy if it fires.
///
/// Called from `drive_step` before each wire turn's compose step. No-ops
/// silently when persona has no compression configured.
pub async fn maybe_compact(
    ctx: &SessionContext,
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
    context_policy: &ContextPolicy,
) -> Result<CompactionOutcome, RuntimeError> {
    // 1. Short-circuit: compression disabled.
    let strategy = match &context_policy.compression {
        None => {
            let (active_len, estimated) = read_history_stats(turn_history)?;
            return Ok(CompactionOutcome::Skipped {
                reason: "compression disabled",
                active_turns: active_len,
                estimated_tokens: estimated,
            });
        }
        Some(s) => s.clone(),
    };

    // 2. Read stats from history (lock, read, release before async work).
    let (active_len, estimated_tokens) = read_history_stats(turn_history)?;

    // 3. Message floor gate.
    let message_floor = context_policy.compress_check_message_floor.unwrap_or(100);
    if active_len < message_floor {
        return Ok(CompactionOutcome::Skipped {
            reason: "below message floor",
            active_turns: active_len,
            estimated_tokens,
        });
    }

    // 4. Build TurnSlice vector from the active history.
    let turns = build_turn_slices(turn_history)?;

    // 5. Compute token threshold.
    let token_threshold = context_policy.compress_token_threshold.unwrap_or_else(|| {
        let max_tokens = ctx.chat_options().max_tokens.unwrap_or(8192) as usize;
        // Conservative fallback: 128k context window.
        let context_window: usize = 128_000;
        let safety_buffer: usize = 8192;
        context_window
            .saturating_sub(max_tokens)
            .saturating_sub(safety_buffer)
    });

    // 6. Async gate: call count_tokens via the provider.
    let (should_fire, token_count) = should_compress(
        ctx.provider().as_ref(),
        &turns,
        ctx.model_id(),
        token_threshold as u64,
    )
    .await
    .map_err(|e| RuntimeError::ProviderError {
        reason: format!("compaction count_tokens failed: {e}"),
    })?;

    if !should_fire {
        return Ok(CompactionOutcome::Skipped {
            reason: "below token threshold",
            active_turns: active_len,
            estimated_tokens: token_count.input_tokens,
        });
    }

    let reported_tokens = token_count.input_tokens;

    // 7. Strategy dispatch.
    let (result, strategy_name) = match strategy {
        CompressionStrategy::Truncate { keep_recent } => {
            let r = apply_truncate(turns, keep_recent, token_threshold as u64, reported_tokens);
            (r, "truncate")
        }
        CompressionStrategy::ImportanceBased {
            keep_recent,
            keep_important,
        } => {
            let r = apply_importance_based(
                turns,
                keep_recent,
                keep_important,
                &ImportanceScoringConfig::default(),
                token_threshold as u64,
                reported_tokens,
            );
            (r, "importance_based")
        }
        CompressionStrategy::TimeDecay {
            compress_after_hours,
            min_keep_recent,
        } => {
            let r = apply_time_decay(
                turns,
                compress_after_hours,
                min_keep_recent,
                token_threshold as u64,
                reported_tokens,
            );
            (r, "time_decay")
        }
        CompressionStrategy::RecursiveSummarization {
            chunk_size,
            ref summarization_model,
            ref summarization_prompt,
        } => {
            // Generate summary via provider call.
            let summary_text = generate_summary(
                ctx,
                turn_history,
                chunk_size,
                summarization_model,
                summarization_prompt.as_deref(),
            )
            .await?;

            let r = apply_recursive_summarization(
                turns,
                chunk_size,
                Some(summary_text),
                token_threshold as u64,
                reported_tokens,
            );
            (r, "recursive_summarization")
        }
        // CompressionStrategy is #[non_exhaustive]; future variants
        // should be handled explicitly. For now, treat unknown variants
        // as no-op to avoid crashing on forward-compatible data.
        _ => {
            return Ok(CompactionOutcome::Skipped {
                reason: "unknown compression strategy variant",
                active_turns: active_len,
                estimated_tokens: reported_tokens,
            });
        }
    };

    let archived_count = result.archived_turns.len();
    let summary_written = result.summary.is_some();
    let active_after = result.active_turns.len();

    if archived_count == 0 {
        return Ok(CompactionOutcome::Skipped {
            reason: "strategy archived zero turns (batch integrity)",
            active_turns: active_len,
            estimated_tokens: reported_tokens,
        });
    }

    // 8. Post-strategy: DB + in-memory updates.
    post_strategy_updates(ctx, turn_history, &result, archived_count).await?;

    // 9. Signal a session-UUID rotation so the provider sees a clean session
    // boundary after each compaction cycle. The default no-op impl on
    // ProviderClient makes this safe for test doubles that don't carry session
    // UUID state; PatternGatewayClient overrides it to call
    // SessionUuidRotator::rotate.
    ctx.provider().rotate_session_uuid();
    tracing::debug!("session UUID rotated after compaction");

    Ok(CompactionOutcome::Fired {
        strategy_name,
        archived_turn_count: archived_count,
        summary_written,
        active_after,
    })
}

// ---- helpers ----------------------------------------------------------------

/// Read active_len and estimated_tokens from TurnHistory under a brief lock.
fn read_history_stats(
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
) -> Result<(usize, u64), RuntimeError> {
    let hist = turn_history
        .lock()
        .map_err(|_| RuntimeError::ProviderError {
            reason: "turn_history mutex poisoned".into(),
        })?;
    Ok((hist.active_len(), hist.estimated_tokens()))
}

/// Build `TurnSlice` records from the active turns in TurnHistory.
fn build_turn_slices(
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
) -> Result<Vec<TurnSlice>, RuntimeError> {
    let hist = turn_history
        .lock()
        .map_err(|_| RuntimeError::ProviderError {
            reason: "turn_history mutex poisoned".into(),
        })?;

    let slices: Vec<TurnSlice> = hist
        .iter_active()
        .map(|tr| {
            // Flatten input + output messages into ChatMessage list.
            let messages: Vec<pattern_core::types::provider::ChatMessage> = tr
                .input
                .messages
                .iter()
                .chain(tr.output.messages.iter())
                .map(|m| m.chat_message.clone())
                .collect();

            let started_at = tr
                .input
                .messages
                .first()
                .map(|m| m.created_at)
                .or_else(|| tr.output.messages.first().map(|m| m.created_at))
                .unwrap_or_else(Timestamp::now);

            TurnSlice {
                ordering_key: tr.turn_id.to_string(),
                batch_id: tr.input.batch_id.clone(),
                messages,
                started_at,
            }
        })
        .collect();

    Ok(slices)
}

/// Generate a summary via provider.complete for RecursiveSummarization.
async fn generate_summary(
    ctx: &SessionContext,
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
    chunk_size: usize,
    summarization_model: &str,
    summarization_prompt: Option<&str>,
) -> Result<String, RuntimeError> {
    use pattern_core::types::provider::{ChatMessage, ChatStreamEvent, CompletionRequest};

    // Build the oldest chunk_size turns' messages.
    let oldest_messages: Vec<ChatMessage> = {
        let hist = turn_history
            .lock()
            .map_err(|_| RuntimeError::ProviderError {
                reason: "turn_history mutex poisoned".into(),
            })?;
        hist.iter_active()
            .take(chunk_size)
            .flat_map(|tr| {
                tr.input
                    .messages
                    .iter()
                    .chain(tr.output.messages.iter())
                    .map(|m| m.chat_message.clone())
            })
            .collect()
    };

    let system = summarization_prompt.unwrap_or(DEFAULT_SUMMARIZATION_SYSTEM_PROMPT);

    let mut messages = oldest_messages;
    messages.push(ChatMessage::user(
        DEFAULT_SUMMARIZATION_DIRECTIVE.to_string(),
    ));

    let req = CompletionRequest::new(summarization_model)
        .with_system(system)
        .with_messages(messages);

    let mut stream =
        ctx.provider()
            .complete(req)
            .await
            .map_err(|e| RuntimeError::ProviderError {
                reason: format!("summarization complete() failed: {e}"),
            })?;

    let mut summary_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| RuntimeError::ProviderError {
            reason: format!("summarization stream error: {e}"),
        })?;
        match event {
            ChatStreamEvent::Chunk(c) => summary_text.push_str(&c.content),
            ChatStreamEvent::End(end) => {
                // If captured_content is available, prefer it (complete text).
                if let Some(content) = end.captured_content
                    && let Some(text) = content.joined_texts()
                {
                    summary_text = text;
                }
            }
            ChatStreamEvent::ToolCallChunk(_) => {
                return Err(RuntimeError::ProviderError {
                    reason: "summarization model produced a tool call — this is unexpected; \
                             the summarizer should produce text only"
                        .into(),
                });
            }
            // Ignore Start, ReasoningChunk, etc.
            _ => {}
        }
    }

    if summary_text.is_empty() {
        return Err(RuntimeError::ProviderError {
            reason: "summarization model returned empty text".into(),
        });
    }

    Ok(summary_text)
}

/// Post-strategy: mark archived messages in DB, write summary if present,
/// update TurnHistory.
async fn post_strategy_updates(
    ctx: &SessionContext,
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
    result: &pattern_provider::compose::compression::CompressionResult,
    archived_count: usize,
) -> Result<(), RuntimeError> {
    // Compute boundary position: the smallest position among the first
    // active (kept) turn's messages. Messages with position < boundary
    // get archived.
    let before_position = compute_archive_boundary(turn_history, archived_count)?;

    // Get a DB connection for the archive operations.
    let conn = ctx.db().get().map_err(|e| RuntimeError::ProviderError {
        reason: format!("db connection failed: {e}"),
    })?;

    // Archive messages in DB.
    pattern_db::queries::archive_messages(&conn, ctx.agent_id(), &before_position).map_err(
        |e| RuntimeError::ProviderError {
            reason: format!("archive_messages failed: {e}"),
        },
    )?;

    // Write summary row if present (RecursiveSummarization).
    if let Some(ref summary_text) = result.summary {
        let (start_position, end_position, message_count) =
            compute_summary_positions(turn_history, archived_count)?;

        let summary = pattern_db::models::ArchiveSummary {
            id: new_snowflake_id().to_string(),
            agent_id: ctx.agent_id().to_string(),
            summary: summary_text.clone(),
            start_position,
            end_position,
            message_count: message_count as i64,
            previous_summary_id: None,
            depth: 0,
            created_at: Timestamp::now(),
        };
        pattern_db::queries::create_archive_summary(&conn, &summary).map_err(|e| {
            RuntimeError::ProviderError {
                reason: format!("create_archive_summary failed: {e}"),
            }
        })?;
    }

    // Reload summary head from DB.
    let head = pattern_db::queries::get_summary_head(&conn, ctx.agent_id()).map_err(|e| {
        RuntimeError::ProviderError {
            reason: format!("get_summary_head failed: {e}"),
        }
    })?;

    // Update in-memory TurnHistory.
    {
        let mut hist = turn_history
            .lock()
            .map_err(|_| RuntimeError::ProviderError {
                reason: "turn_history mutex poisoned".into(),
            })?;
        hist.set_summary_head(head);
        hist.take_oldest(archived_count);
    }

    Ok(())
}

/// Compute the archive boundary position: the smallest position among the
/// first kept turn's messages. We use the position of the first message in
/// the (archived_count)th turn record (i.e., the first kept turn).
///
/// # Edge case: first kept turn has empty messages
///
/// A synthetic or malformed continuation turn may have no messages in either
/// `input.messages` or `output.messages`, giving `min_pos = None`. In that
/// case we fall through to the same "archive everything up through the last
/// archived turn" branch used when no kept turn exists at all. This prevents
/// `compute_archive_boundary` from silently returning `""`, which would make
/// `archive_messages` match no rows and leave the context window unbounded.
fn compute_archive_boundary(
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
    archived_count: usize,
) -> Result<String, RuntimeError> {
    let hist = turn_history
        .lock()
        .map_err(|_| RuntimeError::ProviderError {
            reason: "turn_history mutex poisoned".into(),
        })?;

    // The first kept turn is at index `archived_count`.
    let first_kept = hist.iter_active().nth(archived_count);
    if let Some(tr) = first_kept {
        // Use the smallest position among all messages in this turn.
        let min_pos = tr
            .input
            .messages
            .iter()
            .chain(tr.output.messages.iter())
            .map(|m| m.position.as_str())
            .min();
        if let Some(pos) = min_pos {
            return Ok(pos.to_string());
        }
        // `min_pos` is None: the kept turn has no messages at all (synthetic
        // or malformed continuation turn). Fall through to the archive-all
        // branch below so we don't silently return `""` and miss archiving.
    }

    // Fallback: if no kept turn exists (or the kept turn had no messages),
    // use a position beyond the last archived turn's messages (archive
    // everything up through the archived chunk).
    if let Some(last_archived) = hist.iter_active().nth(archived_count.saturating_sub(1)) {
        let max_pos = last_archived
            .input
            .messages
            .iter()
            .chain(last_archived.output.messages.iter())
            .map(|m| m.position.as_str())
            .max();
        if let Some(pos) = max_pos {
            // Append a character to make position strictly greater than any
            // message in the archived chunk.
            return Ok(format!("{pos}~"));
        }
    }

    // Should not happen if archived_count > 0 and turns have messages,
    // but surface a clear error rather than returning `""` silently.
    Err(RuntimeError::CompactionInternalError {
        reason: "compute_archive_boundary: no message positions found in \
                 archived or kept turns; cannot determine archive boundary"
            .into(),
    })
}

/// Compute (start_position, end_position, message_count) across the
/// archived turns for summary metadata.
fn compute_summary_positions(
    turn_history: &Arc<std::sync::Mutex<TurnHistory>>,
    archived_count: usize,
) -> Result<(String, String, usize), RuntimeError> {
    let hist = turn_history
        .lock()
        .map_err(|_| RuntimeError::ProviderError {
            reason: "turn_history mutex poisoned".into(),
        })?;

    let mut all_positions: Vec<&str> = Vec::new();
    let mut msg_count = 0usize;

    for tr in hist.iter_active().take(archived_count) {
        for m in tr.input.messages.iter().chain(tr.output.messages.iter()) {
            all_positions.push(m.position.as_str());
            msg_count += 1;
        }
    }

    let start = all_positions.iter().min().unwrap_or(&"").to_string();
    let end = all_positions.iter().max().unwrap_or(&"").to_string();

    Ok((start, end, msg_count))
}
