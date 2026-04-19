//! Compression strategies for managing context-window size.
//!
//! # Overview
//!
//! When an agent's active turn history grows large enough to threaten the
//! provider's context-window budget, one of these strategies selects which
//! turns to archive. The caller is responsible for actually writing the
//! archival records to `pattern_db` and updating the summary-head cache in
//! the `TurnHistory` type from `pattern_runtime::memory` — this module only *selects*
//! which turns to keep vs. archive, and provides the async `should_compress`
//! gate that calls the provider for a real token count rather than using a
//! word-count heuristic.
//!
//! # Gate vs. ranking heuristics
//!
//! The *gate* (`should_compress`) uses `ProviderClient::count_tokens` — an
//! async, provider-reported count — to decide whether compression is needed
//! at all. Internal ranking heuristics (e.g. `ImportanceBased` scoring) use
//! cheap char/word-based approximations; they are never used for the
//! compress/don't-compress threshold decision.
//!
//! # Batch integrity (AC8.4)
//!
//! A `MessageBatch` (from `pattern_db::models::message`) groups all `Message`s
//! produced during a single `Session::step` activation under the same `batch_id`.
//! These messages form an atomic unit — partially archiving a batch (keeping
//! some messages while archiving others) would break the tool-call/response
//! pairing invariant and corrupt downstream composers.
//!
//! Every strategy in this module preserves batch integrity: if the
//! compression boundary falls mid-batch, the cut is extended to the nearest
//! whole-batch boundary (compress the entire batch or leave it entirely).
//!
//! This invariant is maintained at the `TurnRecord` level (from `pattern_runtime::memory`):
//! one `TurnRecord` corresponds to one wire-level turn, and all records sharing
//! the same `batch_id` within a `Session::step` are kept or archived
//! together. `find_batch_safe_cut` implements the boundary extension.
//!
//! # Pseudo-message ordering
//!
//! When compaction runs mid-history, `[memory:updated]` pseudo-messages that
//! bracket real messages at specific turn boundaries must retain their
//! relative ordering. Since pseudo-messages are synthesised at compose time
//! from `BlockWrite` records (from `pattern_runtime`) stored in the `block_writes`
//! field of `TurnOutput`, and `TurnRecord`s are kept intact (never split),
//! ordering is automatically preserved as long as the strategy does not
//! reorder `TurnRecord`s. All four strategies return turns in chronological order.

use jiff::Timestamp;
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::ProviderClient;
use pattern_core::types::ids::BatchId;
use pattern_core::types::provider::{ChatMessage, CompletionRequest, TokenCount};
use serde::{Deserialize, Serialize};
use tracing::instrument;

/// A turn record with its ordering key.
///
/// Mirrors the shape of `pattern_runtime::memory::TurnRecord` but is
/// defined here to avoid a cross-crate dependency. The caller maps from
/// the runtime type to this one before calling a strategy.
#[derive(Debug, Clone)]
pub struct TurnSlice {
    /// Stable ordering key for the turn (e.g. a `TurnId` or a position
    /// counter). Used only for chronological sorting; exact type is opaque.
    pub ordering_key: String,
    /// `BatchId` of the `Session::step` this turn belongs to.
    /// All turns from one step share the same id.
    pub batch_id: BatchId,
    /// Flat list of all `ChatMessage`s produced during this turn
    /// (assistant replies, tool results, etc.), in emission order.
    /// Used by `should_compress` to build the token-counting request.
    pub messages: Vec<ChatMessage>,
    /// Wall-clock time of the first message in this turn.
    /// Used by `TimeDecay` to classify old vs. recent turns.
    pub started_at: Timestamp,
}

// CompressionStrategy now lives in pattern_core::types::compression so
// PersonaSnapshot can carry it without a cross-crate cycle. Re-exported
// here for the benefit of callers that already `use
// pattern_provider::compose::compression::CompressionStrategy`.
pub use pattern_core::types::compression::CompressionStrategy;

/// Default *system* prompt for the recursive-summarization strategy
/// when the persona's
/// [`CompressionStrategy::RecursiveSummarization::summarization_prompt`]
/// is `None`. Ported verbatim from v2's compression path (see
/// `rewrite-staging/context/compression.rs` for the original).
///
/// Pairs with [`DEFAULT_SUMMARIZATION_DIRECTIVE`], which the driver
/// appends as a user-message directive after the chunk-of-turns
/// payload.
pub const DEFAULT_SUMMARIZATION_SYSTEM_PROMPT: &str =
    "You are a helpful assistant that creates concise summaries of conversations.";

/// Default *user-message directive* appended to the summarization
/// request after the chunk-of-turns payload. Ported verbatim from v2.
///
/// The persona's `summarization_prompt` override (if any) replaces the
/// system prompt only; the directive is always present so the
/// summarizer has explicit preserve/condense/prioritize/remove
/// guidance. Voice matches Pattern's agent-context use case
/// (relationship-aware, crisis-aware, boundary-aware).
pub const DEFAULT_SUMMARIZATION_DIRECTIVE: &str = "\
Please summarize all the previous messages, focusing on key information, \
decisions made, and important context.

preserve: novel insights, unique terminology we've developed, \
relationship evolution patterns, crisis response validations, \
architectural discoveries

condense: repetitive status updates, routine sync confirmations, similar \
conversations that don't add new dimensions

prioritize: things that would affect future interactions - social \
calibration lessons learned, boundary discoveries, successful \
collaboration patterns, failure modes identified

remove: duplicate information, overly detailed play-by-plays of routine \
events

If there was a previous summary provided, build upon it, but don't \
simply extend it. Maintain the conversational style and preserve \
important details. Keep it as short as reasonable.";

/// Output of a compression run.
///
/// Callers are responsible for writing `archived_turns` to `pattern_db`
/// and replacing the summary head when `summary` is `Some`. The
/// `active_turns` are what remain in the agent's live context.
#[derive(Debug)]
pub struct CompressionResult {
    /// Turns that remain in the active context, in chronological order.
    pub active_turns: Vec<TurnSlice>,
    /// Turns moved to archival, in chronological order.
    pub archived_turns: Vec<TurnSlice>,
    /// Summary text for recursive-summarization runs. `None` for other
    /// strategies.
    pub summary: Option<String>,
    /// Diagnostic metadata about the run.
    pub metadata: CompressionMetadata,
}

/// Diagnostic metadata about a compression run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionMetadata {
    /// Human-readable name of the strategy that ran.
    pub strategy_used: String,
    /// Total turns before compression.
    pub original_turn_count: usize,
    /// Turns archived this run.
    pub archived_turn_count: usize,
    /// Wall-clock time the run completed.
    pub compression_time: jiff::Timestamp,
    /// Token budget that triggered compression (`context_window -
    /// max_output - explicit_buffer`).
    pub budget_tokens: u64,
    /// Provider-reported token count that exceeded the budget.
    pub reported_tokens: u64,
}

/// Configuration for importance scoring (used by `ImportanceBased`).
///
/// All weight fields are additive bonuses applied to each turn's score.
/// The strategy retains turns with the highest total scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportanceScoringConfig {
    /// Base weight for assistant-role messages (default: 3.0).
    pub assistant_weight: f32,
    /// Base weight for user-role messages (default: 5.0).
    pub user_weight: f32,
    /// Base weight for tool-role messages (default: 2.0).
    pub tool_weight: f32,
    /// Maximum recency bonus for the newest of the older turns
    /// (default: 5.0).
    pub recency_bonus: f32,
    /// Bonus per 100 characters of content, capped at 3.0 × this
    /// value (default: 1.0).
    pub content_length_weight: f32,
    /// Bonus for messages containing a `?` (default: 2.0).
    pub question_bonus: f32,
    /// Bonus for messages that contain a tool call (default: 4.0).
    pub tool_call_bonus: f32,
    /// Additional keywords whose presence boosts importance.
    pub important_keywords: Vec<String>,
    /// Per-keyword bonus (default: 1.5).
    pub keyword_bonus: f32,
}

impl Default for ImportanceScoringConfig {
    fn default() -> Self {
        Self {
            assistant_weight: 3.0,
            user_weight: 5.0,
            tool_weight: 2.0,
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

// ---- Gate ---------------------------------------------------------------

/// Returns `true` when the provider-reported input token count for `turns`
/// exceeds `budget_tokens`.
///
/// This is the *only* place in the compression pipeline that calls the
/// provider for a token count. Internal ranking heuristics in strategies
/// like `ImportanceBased` use cheap char-based approximations; they never
/// call this function.
///
/// `model` must be the model string the agent is using (e.g.
/// `"claude-opus-4-7"`). The request is built by concatenating all
/// messages from `turns` in chronological order.
///
/// Budget policy: callers compute `budget_tokens` as
/// `context_window - max_output - explicit_buffer`.
///
/// # Errors
///
/// Propagates [`ProviderError::TokenCountFailed`] from the provider
/// call. Callers may choose to fall back to a heuristic rather than
/// failing hard when the provider is unavailable; this function does
/// not make that choice.
#[instrument(skip(client, turns), fields(turn_count = turns.len(), budget_tokens))]
pub async fn should_compress(
    client: &dyn ProviderClient,
    turns: &[TurnSlice],
    model: &str,
    budget_tokens: u64,
) -> Result<(bool, TokenCount), ProviderError> {
    // Build a minimal CompletionRequest whose messages are the
    // concatenation of all active turns in chronological order.
    let messages: Vec<ChatMessage> = turns
        .iter()
        .flat_map(|t| t.messages.iter().cloned())
        .collect();

    let request = CompletionRequest::new(model).with_messages(messages);

    let count = client.count_tokens(&request).await?;
    tracing::debug!(
        input_tokens = count.input_tokens,
        budget_tokens,
        "should_compress: token count result"
    );
    Ok((count.input_tokens > budget_tokens, count))
}

// ---- Batch-integrity helper -------------------------------------------

/// Find a batch-safe cut point at or before `desired_cut`.
///
/// A "batch-safe" cut never splits a `batch_id` group across the
/// archive/keep boundary. If `desired_cut` falls mid-batch, the cut is
/// moved back to the last turn that belongs to the preceding distinct
/// `batch_id` group. If there is no preceding distinct group, returns 0
/// (keep everything; do not archive a partial batch).
///
/// `turns` must be in chronological order.
pub fn find_batch_safe_cut(turns: &[TurnSlice], desired_cut: usize) -> usize {
    if desired_cut == 0 || turns.is_empty() {
        return 0;
    }
    let cut = desired_cut.min(turns.len());

    // The batch_id at the cut boundary (the first "keep" turn).
    let boundary_batch = if cut < turns.len() {
        Some(&turns[cut].batch_id)
    } else {
        // cut == turns.len() — archive everything; no split possible.
        return cut;
    };

    // Walk backwards from `cut - 1` until we find a turn whose
    // batch_id differs from `boundary_batch`.
    let mut safe_cut = cut;
    for i in (0..cut).rev() {
        if &turns[i].batch_id == boundary_batch.unwrap() {
            // This archived turn shares a batch_id with the first "keep"
            // turn; pull the cut back to before it.
            safe_cut = i;
        } else {
            break;
        }
    }
    safe_cut
}

// ---- Heuristic helpers (ranking-only; never used for the gate) ---------

/// Heuristic message size approximation: character count ÷ 4.
///
/// Used only for ranking within `ImportanceBased`. The gate always
/// uses the provider's real `count_tokens` call.
fn estimate_chars(msg: &ChatMessage) -> usize {
    msg.content.joined_texts().map(|t| t.len()).unwrap_or(0)
}

/// Score a turn's importance for `ImportanceBased` selection.
///
/// Returns a non-negative float; higher is more important. The
/// position `idx` within the older-turn window and `total` are used
/// for a recency bonus so the most-recent older turns are preferred
/// when scores are otherwise similar.
pub fn score_turn(
    turn: &TurnSlice,
    idx: usize,
    total: usize,
    config: &ImportanceScoringConfig,
) -> f32 {
    let mut score = 0.0f32;
    let mut char_total = 0usize;
    let mut has_tool_content = false;

    for msg in &turn.messages {
        use genai::chat::ChatRole;
        let role_weight = match msg.role {
            ChatRole::Assistant => config.assistant_weight,
            ChatRole::User => config.user_weight,
            ChatRole::Tool => config.tool_weight,
            _ => 1.0,
        };
        score += role_weight;

        let chars = estimate_chars(msg);
        char_total += chars;

        if let Some(text) = msg.content.joined_texts() {
            if text.contains('?') {
                score += config.question_bonus;
            }
            let text_lower = text.to_lowercase();
            for kw in &config.important_keywords {
                if text_lower.contains(kw.as_str()) {
                    score += config.keyword_bonus;
                }
            }
        }

        // Tool messages indicate tool-call involvement; the whole turn
        // gets the bonus.
        if msg.role == ChatRole::Tool {
            has_tool_content = true;
        }
    }

    // Content-length bonus.
    let length_factor = (char_total as f32 / 100.0).min(3.0);
    score += length_factor * config.content_length_weight;

    if has_tool_content {
        score += config.tool_call_bonus;
    }

    // Recency bonus within the older-turn window.
    if total > 0 {
        let recency_factor = idx as f32 / total as f32;
        score += recency_factor * config.recency_bonus;
    }

    score
}

// ---- Strategy implementations ------------------------------------------

/// Apply the `Truncate` strategy: archive all but the `keep_recent`
/// most-recent turns. Batch integrity is enforced at the cut boundary.
///
/// Returns a [`CompressionResult`] describing what was kept and what
/// was archived.
pub fn apply_truncate(
    turns: Vec<TurnSlice>,
    keep_recent: usize,
    budget_tokens: u64,
    reported_tokens: u64,
) -> CompressionResult {
    let original_turn_count = turns.len();

    // Desired cut: archive everything before `cut`.
    let desired_cut = original_turn_count.saturating_sub(keep_recent);
    let safe_cut = find_batch_safe_cut(&turns, desired_cut);

    let (to_archive, to_keep) = turns.split_at(safe_cut);
    let archived_turn_count = to_archive.len();

    CompressionResult {
        archived_turns: to_archive.to_vec(),
        active_turns: to_keep.to_vec(),
        summary: None,
        metadata: CompressionMetadata {
            strategy_used: "truncate".to_string(),
            original_turn_count,
            archived_turn_count,
            compression_time: Timestamp::now(),
            budget_tokens,
            reported_tokens,
        },
    }
}

/// Apply the `ImportanceBased` strategy.
///
/// Keeps the `keep_recent` most-recent turns unconditionally, then
/// scores the older turns and retains the top-`keep_important`
/// highest-scoring ones. All others are archived.
pub fn apply_importance_based(
    turns: Vec<TurnSlice>,
    keep_recent: usize,
    keep_important: usize,
    scoring_config: &ImportanceScoringConfig,
    budget_tokens: u64,
    reported_tokens: u64,
) -> CompressionResult {
    let original_turn_count = turns.len();

    if turns.len() <= keep_recent {
        // Nothing old enough to consider compressing.
        return CompressionResult {
            archived_turns: vec![],
            active_turns: turns,
            summary: None,
            metadata: CompressionMetadata {
                strategy_used: "importance_based".to_string(),
                original_turn_count,
                archived_turn_count: 0,
                compression_time: Timestamp::now(),
                budget_tokens,
                reported_tokens,
            },
        };
    }

    let split_at = original_turn_count - keep_recent;
    // Safe: we just checked turns.len() > keep_recent
    let (older, recent) = turns.split_at(split_at);

    let total = older.len();
    let mut scored: Vec<(f32, usize)> = older
        .iter()
        .enumerate()
        .map(|(idx, turn)| (score_turn(turn, idx, total, scoring_config), idx))
        .collect();

    // Sort by descending score to find the most important.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Collect the indices of the `keep_important` highest-scoring turns.
    let mut keep_indices: Vec<usize> = scored
        .into_iter()
        .take(keep_important)
        .map(|(_, idx)| idx)
        .collect();
    keep_indices.sort_unstable();

    let mut active_turns: Vec<TurnSlice> = Vec::with_capacity(keep_recent + keep_important);
    let mut archived_turns: Vec<TurnSlice> = Vec::new();

    let mut keep_set = std::collections::HashSet::new();
    for &i in &keep_indices {
        keep_set.insert(i);
    }

    for (idx, turn) in older.iter().enumerate() {
        if keep_set.contains(&idx) {
            active_turns.push(turn.clone());
        } else {
            archived_turns.push(turn.clone());
        }
    }

    // Apply batch integrity to archived turns: if any archived turn shares
    // a batch_id with a kept turn, move it back to active.
    let active_batch_ids: std::collections::HashSet<&BatchId> =
        active_turns.iter().map(|t| &t.batch_id).collect();

    let mut rescued: Vec<TurnSlice> = vec![];
    archived_turns.retain(|t| {
        if active_batch_ids.contains(&t.batch_id) {
            rescued.push(t.clone());
            false
        } else {
            true
        }
    });
    active_turns.extend(rescued);

    // Append the unconditionally-kept recent turns.
    active_turns.extend_from_slice(recent);

    // Sort active turns back to chronological order using ordering_key.
    active_turns.sort_by(|a, b| a.ordering_key.cmp(&b.ordering_key));
    archived_turns.sort_by(|a, b| a.ordering_key.cmp(&b.ordering_key));

    let archived_turn_count = archived_turns.len();
    CompressionResult {
        archived_turns,
        active_turns,
        summary: None,
        metadata: CompressionMetadata {
            strategy_used: "importance_based".to_string(),
            original_turn_count,
            archived_turn_count,
            compression_time: Timestamp::now(),
            budget_tokens,
            reported_tokens,
        },
    }
}

/// Apply the `TimeDecay` strategy.
///
/// Archives all turns whose `started_at` is older than `cutoff`, subject
/// to the `min_keep_recent` floor. Incomplete-batch protection is applied
/// at the cut boundary.
pub fn apply_time_decay(
    turns: Vec<TurnSlice>,
    compress_after_hours: f64,
    min_keep_recent: usize,
    budget_tokens: u64,
    reported_tokens: u64,
) -> CompressionResult {
    let original_turn_count = turns.len();

    // Compute the cutoff as a Timestamp.
    let cutoff = {
        use jiff::ToSpan;
        let millis = (compress_after_hours * 3600.0 * 1000.0) as i64;
        Timestamp::now()
            .checked_sub(millis.milliseconds())
            .unwrap_or(Timestamp::UNIX_EPOCH)
    };

    // Find the index where turns transition from "old" to "recent" (old
    // turns are those whose `started_at` is before the cutoff). Since
    // turns are in chronological order, the old turns are a prefix.
    let old_count = turns.iter().take_while(|t| t.started_at < cutoff).count();

    // The minimum recent floor: we must keep at least min_keep_recent
    // turns regardless of age.
    let max_archivable = original_turn_count.saturating_sub(min_keep_recent);
    let desired_cut = old_count.min(max_archivable);
    let safe_cut = find_batch_safe_cut(&turns, desired_cut);

    let (to_archive, to_keep) = turns.split_at(safe_cut);
    let archived_turn_count = to_archive.len();

    CompressionResult {
        archived_turns: to_archive.to_vec(),
        active_turns: to_keep.to_vec(),
        summary: None,
        metadata: CompressionMetadata {
            strategy_used: "time_decay".to_string(),
            original_turn_count,
            archived_turn_count,
            compression_time: Timestamp::now(),
            budget_tokens,
            reported_tokens,
        },
    }
}

/// Apply the `RecursiveSummarization` strategy (structure only).
///
/// Archives the oldest `chunk_size` turns (respecting batch integrity).
/// Returns the archived turns in `archived_turns` and stores the
/// caller-provided `summary` in the result. The caller is responsible
/// for actually calling the provider to generate the summary text and
/// passing it in here.
///
/// This function is synchronous; the async summarization call lives in
/// the caller (compaction driver, Phase 6). The `summary` argument
/// carries the result of that call back into the result shape.
pub fn apply_recursive_summarization(
    turns: Vec<TurnSlice>,
    chunk_size: usize,
    summary: Option<String>,
    budget_tokens: u64,
    reported_tokens: u64,
) -> CompressionResult {
    let original_turn_count = turns.len();

    // Archive at most one chunk worth of turns from the oldest end.
    let desired_cut = chunk_size.min(original_turn_count);
    let safe_cut = find_batch_safe_cut(&turns, desired_cut);

    let (to_archive, to_keep) = turns.split_at(safe_cut);
    let archived_turn_count = to_archive.len();

    CompressionResult {
        archived_turns: to_archive.to_vec(),
        active_turns: to_keep.to_vec(),
        summary,
        metadata: CompressionMetadata {
            strategy_used: "recursive_summarization".to_string(),
            original_turn_count,
            archived_turn_count,
            compression_time: Timestamp::now(),
            budget_tokens,
            reported_tokens,
        },
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use genai::chat::ChatMessage;
    use jiff::Timestamp;
    use pattern_core::error::ProviderError;
    use pattern_core::traits::provider_client::{ChunkStream, ProviderClient};
    use pattern_core::types::ids::{BatchId, new_snowflake_id};
    use pattern_core::types::provider::{CompletionRequest, TokenCount};

    use super::*;

    // ---- mock provider -------------------------------------------------------

    /// Mock `ProviderClient` that returns a fixed token count.
    #[derive(Debug)]
    struct MockTokenCounter {
        token_count: u64,
    }

    impl MockTokenCounter {
        fn returning(token_count: u64) -> Arc<Self> {
            Arc::new(Self { token_count })
        }
    }

    #[async_trait]
    impl ProviderClient for MockTokenCounter {
        async fn complete(&self, _r: CompletionRequest) -> Result<ChunkStream, ProviderError> {
            // Phase 5: test-only mock; compression tests need count_tokens, not
            // complete. Intentionally left unimplemented for this mock.
            unimplemented!("mock: count_tokens only")
        }

        async fn count_tokens(&self, _r: &CompletionRequest) -> Result<TokenCount, ProviderError> {
            Ok(TokenCount {
                input_tokens: self.token_count,
            })
        }
    }

    // ---- helpers -------------------------------------------------------------

    fn make_turn(batch_id: BatchId, ordering_key: &str) -> TurnSlice {
        make_turn_with_msg(
            batch_id,
            ordering_key,
            ChatMessage::user("hello"),
            Timestamp::now(),
        )
    }

    fn make_turn_with_msg(
        batch_id: BatchId,
        ordering_key: &str,
        msg: ChatMessage,
        started_at: Timestamp,
    ) -> TurnSlice {
        TurnSlice {
            ordering_key: ordering_key.to_string(),
            batch_id,
            messages: vec![msg],
            started_at,
        }
    }

    fn make_batch_id() -> BatchId {
        BatchId::from(new_snowflake_id())
    }

    // ---- should_compress gate tests -----------------------------------------

    #[tokio::test]
    async fn gate_returns_false_when_under_budget() {
        let client = MockTokenCounter::returning(100);
        let turns = vec![make_turn(make_batch_id(), "t1")];
        let (compress, count) = should_compress(client.as_ref(), &turns, "claude-opus-4-7", 200)
            .await
            .unwrap();
        assert!(!compress, "100 tokens < 200 budget should not compress");
        assert_eq!(count.input_tokens, 100);
    }

    #[tokio::test]
    async fn gate_returns_true_when_over_budget() {
        let client = MockTokenCounter::returning(500);
        let turns = vec![make_turn(make_batch_id(), "t1")];
        let (compress, count) = should_compress(client.as_ref(), &turns, "claude-opus-4-7", 200)
            .await
            .unwrap();
        assert!(compress, "500 tokens > 200 budget should compress");
        assert_eq!(count.input_tokens, 500);
    }

    #[tokio::test]
    async fn gate_sends_all_messages_from_all_turns() {
        // The mock returns a count equal to the message content length
        // divided by something — but we just verify the function
        // assembles and dispatches without panicking when multiple turns
        // and messages are present.
        let client = MockTokenCounter::returning(1000);
        let batch = make_batch_id();
        let turns = vec![
            make_turn_with_msg(
                batch.clone(),
                "t1",
                ChatMessage::user("message one"),
                Timestamp::now(),
            ),
            make_turn_with_msg(
                make_batch_id(),
                "t2",
                ChatMessage::user("message two"),
                Timestamp::now(),
            ),
        ];
        let result = should_compress(client.as_ref(), &turns, "claude-opus-4-7", 500).await;
        assert!(result.is_ok());
    }

    // ---- find_batch_safe_cut tests ------------------------------------------

    #[test]
    fn safe_cut_returns_desired_when_no_batch_split() {
        // Turns have distinct batch_ids; cut at 2 means archive [0,1].
        let b1 = make_batch_id();
        let b2 = make_batch_id();
        let b3 = make_batch_id();
        let turns = vec![
            make_turn(b1, "t1"),
            make_turn(b2, "t2"),
            make_turn(b3, "t3"),
        ];
        assert_eq!(find_batch_safe_cut(&turns, 2), 2);
    }

    #[test]
    fn safe_cut_extends_back_to_avoid_mid_batch_split() {
        // t1 and t2 share a batch; t3 has its own.
        // Desired cut = 1 (archive t1, keep t2+t3).
        // But t1 and t2 share a batch_id, so the cut must be 0.
        let shared = make_batch_id();
        let b3 = make_batch_id();
        let turns = vec![
            make_turn(shared.clone(), "t1"),
            make_turn(shared.clone(), "t2"),
            make_turn(b3, "t3"),
        ];
        // Desired cut = 1 would archive only t1 and keep t2 — but t1 and
        // t2 share a batch, so the cut must retreat to 0.
        let cut = find_batch_safe_cut(&turns, 1);
        assert_eq!(cut, 0, "cutting mid-batch should retreat to 0");
    }

    #[test]
    fn safe_cut_extends_forward_when_boundary_batch_spans_cut() {
        // t1 has its own batch; t2 and t3 share a batch.
        // Desired cut = 2 (archive t1+t2, keep t3).
        // t2 and t3 share a batch_id, so we must not archive t2.
        // Cut retreats to 1.
        let b1 = make_batch_id();
        let shared = make_batch_id();
        let turns = vec![
            make_turn(b1, "t1"),
            make_turn(shared.clone(), "t2"),
            make_turn(shared.clone(), "t3"),
        ];
        let cut = find_batch_safe_cut(&turns, 2);
        assert_eq!(cut, 1, "cut should retreat to 1 to keep t2+t3 together");
    }

    #[test]
    fn safe_cut_zero_returns_zero() {
        let turns = vec![make_turn(make_batch_id(), "t1")];
        assert_eq!(find_batch_safe_cut(&turns, 0), 0);
    }

    #[test]
    fn safe_cut_at_len_archives_everything() {
        let turns = vec![
            make_turn(make_batch_id(), "t1"),
            make_turn(make_batch_id(), "t2"),
        ];
        assert_eq!(find_batch_safe_cut(&turns, 2), 2);
    }

    // ---- AC8.4: batch integrity test ----------------------------------------

    #[test]
    fn ac8_4_batch_integrity_truncate_never_splits_batch() {
        // Build a history where turns 3+4 share a batch_id.
        // keep_recent = 2: would normally cut at position 3 (keep t4+t5),
        // but t3 and t4 share a batch so we must cut at 2 (keep t3+t4+t5).
        let b1 = make_batch_id();
        let b2 = make_batch_id();
        let b3 = make_batch_id();
        let shared = make_batch_id(); // t3 and t4 share this
        let b5 = make_batch_id();
        let turns = vec![
            make_turn(b1, "t1"),
            make_turn(b2, "t2"),
            make_turn(b3, "t3"),
            make_turn(shared.clone(), "t4"),
            make_turn(shared.clone(), "t5"),
            make_turn(b5, "t6"),
        ];
        // keep_recent = 2: desired_cut = 6 - 2 = 4 (archive t1..t4, keep t5+t6)
        // t4 and t5 share a batch — cut must retreat to 3.
        let result = apply_truncate(turns, 2, 1000, 1500);
        // Archived should be t1, t2, t3 (positions 0-2)
        // Active should be t4, t5, t6 (positions 3-5)
        assert_eq!(result.archived_turns.len(), 3, "should archive 3 turns");
        assert_eq!(result.active_turns.len(), 3, "should keep 3 turns");

        // Verify no batch_id appears in both active and archived.
        let archived_ids: std::collections::HashSet<&BatchId> =
            result.archived_turns.iter().map(|t| &t.batch_id).collect();
        for t in &result.active_turns {
            assert!(
                !archived_ids.contains(&t.batch_id),
                "batch_id {:?} appears in both active and archived",
                t.batch_id
            );
        }
    }

    #[test]
    fn ac8_4_batch_integrity_with_pseudo_message_in_batch() {
        // AC8.4: a batch containing a [memory:updated] pseudo-message
        // (a user-role message with system-reminder content) must be kept
        // or archived as a unit.
        //
        // This test builds a history where turns 2 and 3 share a batch_id
        // — turn 2 carries a real message and turn 3 carries a
        // [memory:updated] pseudo-message. With keep_recent=1 the naive
        // cut would be at position 3 (archive t1+t2+t3, keep t4); since
        // t2 and t3 share a batch_id with a pseudo-message, and the
        // boundary turn t4 has a *different* batch_id, the cut at 3 is
        // safe and both t2 and t3 should be archived together.
        let b_t1 = make_batch_id();
        let pseudo_batch = make_batch_id(); // t2 + t3 share this batch
        let b_t4 = make_batch_id();

        let pseudo_msg = ChatMessage::user(
            "<system-reminder>[memory:updated] block 'notes'...</system-reminder>",
        );

        let turns = vec![
            // Turn 1: simple user-assistant exchange — distinct batch
            make_turn(b_t1, "t1"),
            // Turn 2: real message in the pseudo_batch
            make_turn_with_msg(
                pseudo_batch.clone(),
                "t2",
                ChatMessage::user("What time is it?"),
                Timestamp::now(),
            ),
            // Turn 3: [memory:updated] pseudo-message in the same pseudo_batch
            make_turn_with_msg(pseudo_batch.clone(), "t3", pseudo_msg, Timestamp::now()),
            // Turn 4: recent turn — distinct batch (not reusing b_t1)
            make_turn(b_t4, "t4"),
        ];

        // keep_recent=1: desired_cut = 3 (archive [t1,t2,t3], keep [t4]).
        // The boundary batch at index 3 is b_t4 (unique).
        // find_batch_safe_cut walks back from index 2 (t3 has pseudo_batch)
        // — t4 has b_t4 ≠ pseudo_batch, so no retreat. Cut = 3 stands.
        let result = apply_truncate(turns, 1, 1000, 1500);

        // t2 and t3 must both be archived together (not split).
        let archived_keys: Vec<&str> = result
            .archived_turns
            .iter()
            .map(|t| t.ordering_key.as_str())
            .collect();
        assert!(
            archived_keys.contains(&"t2"),
            "t2 should be archived: {archived_keys:?}"
        );
        assert!(
            archived_keys.contains(&"t3"),
            "t3 (pseudo-message) should be archived with t2: {archived_keys:?}"
        );

        // Verify batch integrity: no batch_id splits across the boundary.
        let archived_ids: std::collections::HashSet<&BatchId> =
            result.archived_turns.iter().map(|t| &t.batch_id).collect();
        for t in &result.active_turns {
            assert!(
                !archived_ids.contains(&t.batch_id),
                "batch {:?} split across active/archived boundary",
                t.batch_id
            );
        }

        // Now test the more complex case: if the cut would fall BETWEEN
        // t2 and t3 (desired_cut=2), find_batch_safe_cut must retreat to 1.
        let b_t1b = make_batch_id();
        let pseudo_batch2 = make_batch_id();
        let b_t4b = make_batch_id();
        let turns2 = vec![
            make_turn(b_t1b, "t1"),
            make_turn_with_msg(
                pseudo_batch2.clone(),
                "t2",
                ChatMessage::user("real"),
                Timestamp::now(),
            ),
            make_turn_with_msg(
                pseudo_batch2.clone(),
                "t3",
                ChatMessage::user("<system-reminder>[memory:updated]</system-reminder>"),
                Timestamp::now(),
            ),
            make_turn(b_t4b, "t4"),
        ];
        // keep_recent=2: desired_cut = 4-2 = 2 (archive [t1,t2], keep [t3,t4]).
        // But t2 and t3 share pseudo_batch2, so cut retreats to 1.
        let result2 = apply_truncate(turns2, 2, 1000, 1500);
        assert_eq!(
            result2.archived_turns.len(),
            1,
            "cut must retreat to 1 to keep t2+t3 together"
        );
        assert_eq!(result2.archived_turns[0].ordering_key, "t1");
        // t2 and t3 must both be in active (kept together).
        let active_keys: Vec<&str> = result2
            .active_turns
            .iter()
            .map(|t| t.ordering_key.as_str())
            .collect();
        assert!(active_keys.contains(&"t2"));
        assert!(active_keys.contains(&"t3"));
    }

    // ---- Pseudo-message ordering test (step 4) ------------------------------

    #[test]
    fn pseudo_message_ordering_preserved_after_truncation() {
        // Verify that after truncation, the chronological order of turns
        // (and thus the pseudo-messages they contain) is preserved.
        // This validates step 4 of the plan: pseudo-messages keep their
        // ordering relative to the real messages they bracketed.
        let b1 = make_batch_id();
        let b2 = make_batch_id();
        let b3 = make_batch_id();
        let b4 = make_batch_id();

        let now = Timestamp::now();
        let turns = vec![
            make_turn_with_msg(
                b1,
                "0001",
                ChatMessage::user("real message 1"),
                now.checked_sub(jiff::ToSpan::seconds(400)).unwrap(),
            ),
            make_turn_with_msg(
                b2,
                "0002",
                ChatMessage::user(
                    "<system-reminder>[memory:updated] block 'persona'</system-reminder>",
                ),
                now.checked_sub(jiff::ToSpan::seconds(300)).unwrap(),
            ),
            make_turn_with_msg(
                b3,
                "0003",
                ChatMessage::user("real message 2"),
                now.checked_sub(jiff::ToSpan::seconds(200)).unwrap(),
            ),
            make_turn_with_msg(
                b4,
                "0004",
                ChatMessage::user(
                    "<system-reminder>[memory:updated] block 'notes'</system-reminder>",
                ),
                now.checked_sub(jiff::ToSpan::seconds(100)).unwrap(),
            ),
        ];

        // keep_recent = 2: archive [0001, 0002], keep [0003, 0004].
        let result = apply_truncate(turns, 2, 1000, 1500);

        // Active turns should be in chronological order (ordering_key sort).
        let active_keys: Vec<&str> = result
            .active_turns
            .iter()
            .map(|t| t.ordering_key.as_str())
            .collect();
        assert_eq!(active_keys, vec!["0003", "0004"]);

        // The [memory:updated] pseudo-message in 0004 must come after the
        // real message in 0003.
        let active_texts: Vec<String> = result
            .active_turns
            .iter()
            .flat_map(|t| &t.messages)
            .filter_map(|m| m.content.joined_texts())
            .collect();
        assert_eq!(
            active_texts[0], "real message 2",
            "real message must precede the pseudo-message"
        );
        assert!(
            active_texts[1].contains("[memory:updated]"),
            "pseudo-message must follow the real message"
        );
    }

    // ---- Truncate strategy tests --------------------------------------------

    #[test]
    fn truncate_archives_oldest_turns() {
        let turns: Vec<TurnSlice> = (0..10)
            .map(|i| make_turn(make_batch_id(), &format!("{i:04}")))
            .collect();
        let result = apply_truncate(turns, 5, 1000, 1500);
        assert_eq!(result.active_turns.len(), 5);
        assert_eq!(result.archived_turns.len(), 5);
        assert_eq!(
            result.active_turns[0].ordering_key, "0005",
            "active must start at turn 5"
        );
    }

    #[test]
    fn truncate_keeps_all_when_fewer_than_keep_recent() {
        let turns: Vec<TurnSlice> = (0..3)
            .map(|i| make_turn(make_batch_id(), &format!("{i:04}")))
            .collect();
        let result = apply_truncate(turns, 10, 1000, 1200);
        assert_eq!(result.active_turns.len(), 3);
        assert_eq!(result.archived_turns.len(), 0);
    }

    // ---- TimeDecay tests ----------------------------------------------------

    #[test]
    fn time_decay_archives_old_turns() {
        use jiff::ToSpan;
        let now = Timestamp::now();
        let old_time = now.checked_sub(3.hours()).unwrap();
        let recent_time = now.checked_sub(10.minutes()).unwrap();

        let mut turns = vec![];
        for i in 0..5 {
            turns.push(make_turn_with_msg(
                make_batch_id(),
                &format!("{i:04}"),
                ChatMessage::user("old"),
                old_time,
            ));
        }
        for i in 5..10 {
            turns.push(make_turn_with_msg(
                make_batch_id(),
                &format!("{i:04}"),
                ChatMessage::user("recent"),
                recent_time,
            ));
        }

        let result = apply_time_decay(turns, 1.0, 2, 1000, 1500);
        // 5 old turns, min_keep_recent = 2, so max_archivable = 8, old_count = 5
        // => desired_cut = 5 (archive 5, keep 5)
        assert_eq!(result.archived_turns.len(), 5);
        assert_eq!(result.active_turns.len(), 5);
        for t in &result.archived_turns {
            assert_eq!(t.messages[0].content.joined_texts().as_deref(), Some("old"));
        }
    }

    #[test]
    fn time_decay_respects_min_keep_recent() {
        use jiff::ToSpan;
        let now = Timestamp::now();
        let old = now.checked_sub(5.hours()).unwrap();
        let turns: Vec<TurnSlice> = (0..5)
            .map(|i| {
                make_turn_with_msg(
                    make_batch_id(),
                    &format!("{i:04}"),
                    ChatMessage::user("old"),
                    old,
                )
            })
            .collect();

        // All 5 turns are old, but min_keep_recent = 3 means we keep 3.
        let result = apply_time_decay(turns, 1.0, 3, 1000, 1500);
        assert_eq!(result.archived_turns.len(), 2);
        assert_eq!(result.active_turns.len(), 3);
    }

    // ---- ImportanceBased tests ----------------------------------------------

    #[test]
    fn importance_based_keeps_high_score_turns() {
        // Use a custom config that disables recency bonus so the keyword
        // match is the deciding factor — makes the test deterministic
        // regardless of index ordering.
        let config = ImportanceScoringConfig {
            recency_bonus: 0.0,
            ..ImportanceScoringConfig::default()
        };

        let b1 = make_batch_id();
        let b2 = make_batch_id();
        let b3 = make_batch_id();
        let b4 = make_batch_id();
        let turns = vec![
            // t1: low-score (no keywords, no question)
            make_turn_with_msg(b1, "t1", ChatMessage::user("hello world"), Timestamp::now()),
            // t2: high-score (two important keywords)
            make_turn_with_msg(
                b2,
                "t2",
                ChatMessage::user("this is very important remember it always"),
                Timestamp::now(),
            ),
            // t3: medium-score (question bonus only)
            make_turn_with_msg(
                b3,
                "t3",
                ChatMessage::user("how are you?"),
                Timestamp::now(),
            ),
            // t4: the "recent" turn always kept
            make_turn_with_msg(b4, "t4", ChatMessage::user("recent turn"), Timestamp::now()),
        ];

        let result = apply_importance_based(turns, 1, 1, &config, 1000, 1500);
        // keep_recent=1 keeps t4; keep_important=1 should keep t2 (highest
        // score due to "important", "remember", and "always" keywords).
        assert_eq!(result.active_turns.len(), 2);
        let active_keys: std::collections::HashSet<&str> = result
            .active_turns
            .iter()
            .map(|t| t.ordering_key.as_str())
            .collect();
        assert!(active_keys.contains("t4"), "recent turn must be kept");
        assert!(active_keys.contains("t2"), "important turn must be kept");
    }

    #[test]
    fn importance_scoring_question_bonus() {
        let config = ImportanceScoringConfig::default();
        let batch = make_batch_id();
        let turn = make_turn_with_msg(
            batch,
            "t1",
            ChatMessage::user("What is the capital of France?"),
            Timestamp::now(),
        );
        let score_with_q = score_turn(&turn, 0, 1, &config);

        let batch2 = make_batch_id();
        let turn_no_q = make_turn_with_msg(
            batch2,
            "t2",
            ChatMessage::user("The capital of France is Paris"),
            Timestamp::now(),
        );
        let score_without_q = score_turn(&turn_no_q, 0, 1, &config);

        assert!(
            score_with_q > score_without_q,
            "question bonus should increase score: {score_with_q} vs {score_without_q}"
        );
    }

    // ---- RecursiveSummarization tests ---------------------------------------

    #[test]
    fn recursive_summarization_archives_one_chunk() {
        let turns: Vec<TurnSlice> = (0..10)
            .map(|i| make_turn(make_batch_id(), &format!("{i:04}")))
            .collect();
        let result =
            apply_recursive_summarization(turns, 3, Some("summary text".into()), 1000, 1500);
        assert_eq!(result.archived_turns.len(), 3);
        assert_eq!(result.active_turns.len(), 7);
        assert_eq!(result.summary.as_deref(), Some("summary text"));
    }

    #[test]
    fn recursive_summarization_respects_batch_integrity() {
        // Turns 2 and 3 share a batch; chunk_size=3 would normally cut at 3,
        // but that would split t3/t4 (zero-indexed t2/t3 if they share batch).
        let b1 = make_batch_id();
        let shared = make_batch_id();
        let b4 = make_batch_id();
        let turns = vec![
            make_turn(b1, "t1"),
            make_turn(shared.clone(), "t2"),
            make_turn(shared.clone(), "t3"), // same batch as t2
            make_turn(b4, "t4"),
        ];
        // chunk_size=2: desired_cut=2, boundary=t3 which shares batch with t2
        // safe_cut retreats to 1.
        let result = apply_recursive_summarization(turns, 2, None, 1000, 1500);
        assert_eq!(
            result.archived_turns.len(),
            1,
            "should only archive t1 to preserve t2+t3 batch"
        );

        // No batch split.
        let archived_ids: std::collections::HashSet<&BatchId> =
            result.archived_turns.iter().map(|t| &t.batch_id).collect();
        for t in &result.active_turns {
            assert!(!archived_ids.contains(&t.batch_id));
        }
    }

    // ---- Serde round-trip ---------------------------------------------------

    #[test]
    fn compression_strategy_serialization_round_trip() {
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
            CompressionStrategy::RecursiveSummarization {
                chunk_size: 5,
                summarization_model: "claude-opus-4-7".into(),
                summarization_prompt: None,
            },
        ];

        for strategy in &strategies {
            let json = serde_json::to_string(strategy).unwrap();
            let back: CompressionStrategy = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2, "serde round-trip failed for {strategy:?}");
        }
    }

    #[test]
    fn importance_scoring_config_round_trip() {
        let config = ImportanceScoringConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let back: ImportanceScoringConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.assistant_weight, back.assistant_weight);
        assert_eq!(config.important_keywords, back.important_keywords);
    }
}
