//! In-memory active turn history + cached archive-summary head.
//!
//! [`TurnHistory`] holds active turns (unbounded at this layer; Task 13's
//! compaction strategies manage size via the context-window-budget-minus-buffer
//! policy), caches the archive-summary head vector loaded from `pattern_db` on
//! session open, and tracks a running `estimated_tokens` count that combines
//! real counts from provider usage with a heuristic fallback.
//!
//! The estimated-token count is always `u64` (no `Option`): callers get a
//! usable number unconditionally, with internal fallback handling missing data.

use std::collections::VecDeque;

use pattern_core::types::block::BlockWrite;
use pattern_core::types::message::Message;
use pattern_core::types::turn::{TurnId, TurnOutput};
use pattern_db::models::ArchiveSummary;

/// Pairs a turn's id with its output for session-retained in-memory history.
#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn_id: TurnId,
    pub output: TurnOutput,
}

/// In-memory active turn history + cached archive-summary head.
/// Unbounded at this layer; Task 13's compaction strategies manage
/// size via the context-window-budget-minus-buffer policy.
#[derive(Debug)]
pub struct TurnHistory {
    active: VecDeque<TurnRecord>,
    /// Cached summary-head vector: one summary per depth level,
    /// newest chronologically. Populated on session open from
    /// pattern_db. Updated by Task 13 after each compaction via
    /// `set_summary_head`. Composer prepends this to segment 2 as
    /// synthesized "earlier context".
    summary_head: Vec<ArchiveSummary>,
    /// Running token-count estimate combining real counts from
    /// provider usage + heuristic fallback for turns without usage
    /// data. Always a u64 (no Option); callers don't handle
    /// "missing data" cases — the heuristic covers it.
    estimated_tokens: u64,
}

impl TurnHistory {
    /// Empty history, for fresh session construction or tests.
    pub fn empty() -> Self {
        Self {
            active: VecDeque::new(),
            summary_head: Vec::new(),
            estimated_tokens: 0,
        }
    }

    /// Load cached summary-head from pattern_db for this agent.
    /// Uses `pattern_db::queries::message::get_summary_head` to
    /// produce one entry per depth level, chronologically ordered.
    pub async fn load(
        db: &pattern_db::ConstellationDb,
        agent_id: &str,
    ) -> Result<Self, pattern_db::error::DbError> {
        let summary_head = pattern_db::queries::get_summary_head(db.pool(), agent_id).await?;
        Ok(Self {
            active: VecDeque::new(),
            summary_head,
            estimated_tokens: 0,
        })
    }

    /// Record a completed turn. Updates estimated_tokens heuristically
    /// using the output's usage (when populated) + heuristic fallback
    /// for the turn's messages. Task 12 populates usage; until then,
    /// fallback is always taken.
    pub fn record(&mut self, turn_id: TurnId, output: TurnOutput) {
        let delta = estimate_turn_tokens(&output);
        self.estimated_tokens = self.estimated_tokens.saturating_add(delta);
        self.active.push_back(TurnRecord { turn_id, output });
    }

    /// Replace the running estimate with an authoritative real count
    /// from provider's count_tokens. Task 13 calls this after periodic
    /// async count_tokens refresh. Accepts u64 (provider-native width).
    pub fn refresh_real_tokens(&mut self, real_count: u64) {
        self.estimated_tokens = real_count;
    }

    /// Always returns u64. Internal fallback handles missing data —
    /// callers don't reason about confidence.
    pub fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    /// Messages from active turns in chronological order. Composer's
    /// Segment 2 pass iterates over this.
    pub fn active_messages(&self) -> impl Iterator<Item = &Message> {
        self.active.iter().flat_map(|tr| tr.output.messages.iter())
    }

    /// Block writes from the immediately-prior turn, used by Segment 2
    /// for pseudo-message emission. Empty if this is the first turn.
    pub fn most_recent_block_writes(&self) -> &[BlockWrite] {
        self.active
            .back()
            .map(|tr| tr.output.block_writes.as_slice())
            .unwrap_or(&[])
    }

    /// Cached archive-summary head. Composer prepends to Segment 2.
    pub fn summary_head(&self) -> &[ArchiveSummary] {
        &self.summary_head
    }

    /// Compaction layer (Task 13) updates the cached head after
    /// generating new archive_summaries rows.
    pub fn set_summary_head(&mut self, head: Vec<ArchiveSummary>) {
        self.summary_head = head;
    }

    /// Compaction takes ownership of oldest N turns. Those turns'
    /// messages are then marked is_archived=1 in pattern_db and folded
    /// into a new archive_summaries row.
    pub fn take_oldest(&mut self, n: usize) -> Vec<TurnRecord> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(tr) = self.active.pop_front() {
                out.push(tr);
            } else {
                break;
            }
        }
        // Recompute estimated_tokens from remaining active via the
        // heuristic. Task 13's next real-count refresh will overwrite.
        self.estimated_tokens = self
            .active
            .iter()
            .map(|tr| estimate_turn_tokens(&tr.output))
            .sum();
        out
    }

    /// All retained turns in chronological order. Task 13's compaction
    /// strategies walk this.
    pub fn iter_active(&self) -> impl Iterator<Item = &TurnRecord> {
        self.active.iter()
    }

    /// Number of active turns currently retained.
    pub fn active_len(&self) -> usize {
        self.active.len()
    }
}

/// Heuristic per-turn token estimate used when real counts aren't
/// available. Rough `chars / 4` on message text plus a small flat
/// overhead per turn. Callers don't see the heuristic; it's internal
/// to `estimated_tokens()`.
fn estimate_turn_tokens(output: &TurnOutput) -> u64 {
    // Prefer provider's real count if present.
    if let Some(ref usage) = output.usage {
        return (usage.prompt_tokens.unwrap_or(0) as u64)
            .saturating_add(usage.completion_tokens.unwrap_or(0) as u64);
    }
    // Heuristic fallback: ~4 chars per token + flat overhead.
    let text_chars: u64 = output
        .messages
        .iter()
        .map(|m| m.chat_message.size() as u64)
        .sum();
    text_chars / 4 + 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::block::BlockWriteKind;
    use pattern_core::types::ids::new_id;
    use pattern_core::types::origin::{AgentAuthor, Author};
    use smol_str::SmolStr;

    fn make_turn_output(msg_count: usize, block_writes: Vec<BlockWrite>) -> TurnOutput {
        use pattern_core::types::turn::StopReason;
        TurnOutput {
            messages: (0..msg_count)
                .map(|i| Message {
                    chat_message: genai::chat::ChatMessage::user(format!("msg {i}")),
                    id: new_id(),
                    owner_id: SmolStr::new("agent-a"),
                    created_at: Timestamp::now(),
                    batch: new_id(),
                    response_meta: None,
                    block_refs: vec![],
                })
                .collect(),
            block_writes,
            tool_calls: vec![],
            tool_results: vec![],
            stop_reason: StopReason::EndTurn,
            usage: None,
            cache_metrics: Default::default(),
            completed_at: Timestamp::now(),
        }
    }

    fn make_block_write(handle: &str) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_01"),
            block_type: pattern_core::memory::BlockType::Working,
            rendered_content: "content".to_string(),
            kind: BlockWriteKind::Created,
            previous_content_hash: None,
            previous_rendered_content: None,
            at: Timestamp::now(),
            author: Author::Agent(AgentAuthor {
                agent_id: SmolStr::new("agent-a"),
            }),
        }
    }

    #[test]
    fn empty_then_record_roundtrip() {
        let mut hist = TurnHistory::empty();
        assert_eq!(hist.active_len(), 0);
        assert!(hist.most_recent_block_writes().is_empty());

        let output = make_turn_output(2, vec![]);
        hist.record(new_id(), output);
        assert_eq!(hist.active_len(), 1);
        assert_eq!(hist.active_messages().count(), 2);
    }

    #[test]
    fn estimated_tokens_accumulates_and_refresh_overwrites() {
        let mut hist = TurnHistory::empty();
        assert_eq!(hist.estimated_tokens(), 0);

        // Record turns with heuristic fallback.
        hist.record(new_id(), make_turn_output(1, vec![]));
        let after_one = hist.estimated_tokens();
        assert!(after_one > 0, "heuristic should produce nonzero count");

        hist.record(new_id(), make_turn_output(1, vec![]));
        let after_two = hist.estimated_tokens();
        assert!(after_two > after_one, "should accumulate");

        // Refresh with authoritative count.
        hist.refresh_real_tokens(999);
        assert_eq!(hist.estimated_tokens(), 999);
    }

    #[test]
    fn most_recent_block_writes_returns_last_turn() {
        let mut hist = TurnHistory::empty();
        assert!(hist.most_recent_block_writes().is_empty());

        // First turn: no block writes.
        hist.record(new_id(), make_turn_output(0, vec![]));
        assert!(hist.most_recent_block_writes().is_empty());

        // Second turn: has block writes.
        let writes = vec![make_block_write("notes"), make_block_write("tasks")];
        hist.record(new_id(), make_turn_output(0, writes));
        assert_eq!(hist.most_recent_block_writes().len(), 2);
        assert_eq!(hist.most_recent_block_writes()[0].handle.as_str(), "notes");
    }

    #[test]
    fn take_oldest_removes_and_recomputes() {
        let mut hist = TurnHistory::empty();
        hist.record(new_id(), make_turn_output(3, vec![]));
        hist.record(new_id(), make_turn_output(3, vec![]));
        hist.record(new_id(), make_turn_output(3, vec![]));
        assert_eq!(hist.active_len(), 3);

        let taken = hist.take_oldest(2);
        assert_eq!(taken.len(), 2);
        assert_eq!(hist.active_len(), 1);
        // estimated_tokens should be recomputed from remaining turn only.
        let remaining_estimate = estimate_turn_tokens(&hist.iter_active().next().unwrap().output);
        assert_eq!(hist.estimated_tokens(), remaining_estimate);
    }

    #[test]
    fn take_oldest_more_than_available() {
        let mut hist = TurnHistory::empty();
        hist.record(new_id(), make_turn_output(1, vec![]));

        let taken = hist.take_oldest(5);
        assert_eq!(taken.len(), 1);
        assert_eq!(hist.active_len(), 0);
        assert_eq!(hist.estimated_tokens(), 0);
    }

    #[test]
    fn set_summary_head_replaces_cache() {
        let mut hist = TurnHistory::empty();
        assert!(hist.summary_head().is_empty());

        let summaries = vec![ArchiveSummary {
            id: "s1".to_string(),
            agent_id: "agent-a".to_string(),
            summary: "old context".to_string(),
            start_position: "001".to_string(),
            end_position: "010".to_string(),
            message_count: 10,
            previous_summary_id: None,
            depth: 0,
            created_at: chrono::Utc::now(),
        }];
        hist.set_summary_head(summaries);
        assert_eq!(hist.summary_head().len(), 1);
        assert_eq!(hist.summary_head()[0].id, "s1");
    }
}
