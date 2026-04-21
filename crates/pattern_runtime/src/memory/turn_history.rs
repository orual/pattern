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

use genai::chat::ChatRole;
use jiff::Timestamp;
use pattern_core::types::block::BlockWrite;
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::provider::ToolCall;
use pattern_core::types::turn::{StopReason, TurnId, TurnInput, TurnOutput};
use pattern_db::models::{ArchiveSummary, BatchType};
use smol_str::SmolStr;

/// Pairs a turn's id with its full round-trip for session-retained in-memory history.
///
/// Stores both the input (user/system messages that triggered this turn) and
/// the output (assistant reply + inlined tool_result message when applicable).
/// The [`TurnHistory::active_messages`] iterator interleaves input and output
/// messages in order so the composer's Segment 2 pass replays the complete
/// conversational context correctly.
#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn_id: TurnId,
    /// Messages delivered to the agent for this activation (the "user" side).
    pub input: TurnInput,
    /// Messages produced by the agent (assistant reply + tool_result if ToolUse).
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
    /// Number of batches recorded since the last Full snapshot was
    /// emitted. Reset to 0 when a Full is emitted; incremented when
    /// a new batch starts (batch_id differs from prior record).
    batches_since_last_full: u32,
    /// Set to `true` by the compaction layer when turns are archived;
    /// consumed (and cleared) by `drive_step` to trigger a Full
    /// snapshot on the next batch.
    post_compaction_pending: bool,
    /// The batch_id of the most recently recorded turn, used to detect
    /// new-batch transitions.
    most_recent_batch_id: Option<BatchId>,
}

impl TurnHistory {
    /// Empty history, for fresh session construction or tests.
    pub fn empty() -> Self {
        Self {
            active: VecDeque::new(),
            summary_head: Vec::new(),
            estimated_tokens: 0,
            batches_since_last_full: 0,
            post_compaction_pending: false,
            most_recent_batch_id: None,
        }
    }

    /// Load cached summary-head and reconstruct active turns from pattern_db.
    ///
    /// 1. Loads summary_head (one entry per depth level).
    /// 2. Queries non-archived messages ordered by position.
    /// 3. Converts each `pattern_db::models::Message` back to a
    ///    `pattern_core::types::message::Message`.
    /// 4. Groups by batch_id, runs turn-boundary detection per batch.
    /// 5. Populates `active` with reconstructed `TurnRecord`s.
    /// 6. Initialises `estimated_tokens` from the reconstructed outputs.
    pub async fn load(
        db: &pattern_db::ConstellationDb,
        agent_id: &str,
    ) -> Result<Self, pattern_db::error::DbError> {
        let conn = db.get()?;
        let summary_head = pattern_db::queries::get_summary_head(&conn, agent_id)?;

        // Query non-archived messages. The query returns DESC order; we
        // reverse to get chronological (ASC by position) order.
        // Use a generous limit to fetch all active messages.
        let mut db_messages = pattern_db::queries::get_messages(&conn, agent_id, i64::MAX)?;
        db_messages.reverse();

        // Convert DB messages to core messages.
        let core_messages: Vec<Message> = db_messages
            .iter()
            .map(db_message_to_core)
            .collect::<Result<Vec<_>, _>>()?;

        // Group by batch_id, maintaining position order within each batch.
        // Use a stable partition: walk messages in order, collecting into
        // per-batch buckets.
        let batches = group_by_batch(core_messages);

        // Build TurnRecords from each batch group.
        let mut active = VecDeque::new();
        // Also track batch_type per batch_id from the DB messages for
        // origin inference.
        let batch_types: std::collections::HashMap<String, BatchType> = db_messages
            .iter()
            .filter_map(|m| {
                let bid = m.batch_id.as_ref()?;
                let bt = m.batch_type?;
                Some((bid.clone(), bt))
            })
            .collect();

        for (batch_id, msgs) in &batches {
            let batch_type = batch_types
                .get(batch_id.as_str())
                .copied()
                .unwrap_or(BatchType::UserRequest);
            let records = build_turn_records_from_batch(batch_id.clone(), msgs.clone(), batch_type);
            active.extend(records);
        }

        // Estimate tokens from reconstructed outputs.
        let estimated_tokens: u64 = active
            .iter()
            .map(|tr| estimate_turn_tokens(&tr.output))
            .sum();

        // Set most_recent_batch_id from the last record.
        let most_recent_batch_id = active.back().map(|tr| tr.input.batch_id.clone());

        Ok(Self {
            active,
            summary_head,
            estimated_tokens,
            batches_since_last_full: 0,
            post_compaction_pending: false,
            most_recent_batch_id,
        })
    }

    /// Record a completed turn's full round-trip (input + output).
    ///
    /// The `input` carries the user/system messages that triggered this turn;
    /// the `output` carries the assistant reply and (on tool-use turns) the
    /// inlined tool_result message. Both are stored atomically in one
    /// [`TurnRecord`] so [`Self::active_messages`] can interleave them
    /// correctly for the composer's Segment 2 pass.
    ///
    /// Updates `estimated_tokens` heuristically using the output's usage (when
    /// populated) + a heuristic fallback for turns without usage data. Task 12
    /// populates usage; until then, the fallback is always taken.
    pub fn record(&mut self, turn_id: TurnId, input: TurnInput, output: TurnOutput) {
        let delta = estimate_turn_tokens(&output);
        self.estimated_tokens = self.estimated_tokens.saturating_add(delta);

        // Detect new-batch transition for snapshot scheduling.
        let is_new_batch = self
            .most_recent_batch_id
            .as_ref()
            .map(|prev| *prev != input.batch_id)
            .unwrap_or(true);
        if is_new_batch {
            self.batches_since_last_full = self.batches_since_last_full.saturating_add(1);
            self.most_recent_batch_id = Some(input.batch_id.clone());
        }

        self.active.push_back(TurnRecord {
            turn_id,
            input,
            output,
        });
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

    /// Messages from active turns in chronological order.
    ///
    /// Interleaves input and output messages per turn so the composer's
    /// Segment 2 pass sees the complete conversational round-trip:
    /// `[turn_0.input, turn_0.output, turn_1.input, turn_1.output, ...]`.
    ///
    /// On a tool-use turn the output contains both the assistant message and
    /// the tool_result message, giving the correct Anthropic wire shape:
    /// `[user_msg, assistant(tool_use), tool_result, user_msg_2, ...]`.
    pub fn active_messages(&self) -> impl Iterator<Item = &Message> {
        self.active
            .iter()
            .flat_map(|tr| tr.input.messages.iter().chain(tr.output.messages.iter()))
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
        if !out.is_empty() {
            // Signal that a compaction occurred — next batch should
            // emit a Full snapshot so the model gets a complete view.
            self.post_compaction_pending = true;
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
    pub fn iter_active(&self) -> impl DoubleEndedIterator<Item = &TurnRecord> {
        self.active.iter()
    }

    /// Number of active turns currently retained.
    pub fn active_len(&self) -> usize {
        self.active.len()
    }

    // ---- Snapshot scheduling accessors ----

    /// Number of batches recorded since the last Full snapshot was emitted.
    pub fn batches_since_last_full(&self) -> u32 {
        self.batches_since_last_full
    }

    /// Whether a compaction has occurred since the last Full snapshot,
    /// signalling that the next batch should emit a Full.
    pub fn post_compaction_pending(&self) -> bool {
        self.post_compaction_pending
    }

    /// Mark that a compaction has occurred. The next batch's snapshot
    /// will be a Full. Consumed by `clear_post_compaction`.
    pub fn set_post_compaction_pending(&mut self) {
        self.post_compaction_pending = true;
    }

    /// Clear the post-compaction flag after a Full snapshot has been
    /// emitted. Also resets `batches_since_last_full` to 0.
    pub fn note_full_snapshot_emitted(&mut self) {
        self.post_compaction_pending = false;
        self.batches_since_last_full = 0;
    }

    /// The batch_id of the most recently recorded turn. Returns `None`
    /// on a fresh (empty) history.
    pub fn most_recent_batch_id(&self) -> Option<&BatchId> {
        self.most_recent_batch_id.as_ref()
    }
}

// ---- Turn history restoration helpers ------------------------------------

/// Convert a `pattern_db::models::Message` back to a `pattern_core::types::message::Message`.
///
/// Reverses the `to_db_message` conversion in `agent_loop.rs`:
/// - `content_json` is deserialized back to `genai::chat::ChatMessage`.
/// - `created_at` is converted from `chrono::DateTime<Utc>` to `jiff::Timestamp`.
/// - Fields not stored in the DB (`response_meta`, `block_refs`, `attachments`)
///   are defaulted to empty/None.
fn db_message_to_core(
    db_msg: &pattern_db::models::Message,
) -> Result<Message, pattern_db::error::DbError> {
    // Deserialize the ChatMessage from the stored JSON value.
    let chat_message: genai::chat::ChatMessage =
        serde_json::from_value(db_msg.content_json.0.clone())?;

    // Convert chrono::DateTime<Utc> → jiff::Timestamp.
    // Reverse of the forward path: epoch_nanos = secs * 1e9 + nanos.
    let secs = db_msg.created_at.timestamp();
    let nanos = db_msg.created_at.timestamp_subsec_nanos() as i64;
    let epoch_nanos: i128 = (secs as i128) * 1_000_000_000 + (nanos as i128);
    let created_at = Timestamp::from_nanosecond(epoch_nanos).unwrap_or_else(|_| Timestamp::now());

    let batch = db_msg
        .batch_id
        .as_deref()
        .map(SmolStr::new)
        .unwrap_or_else(|| SmolStr::new("unknown"));

    Ok(Message {
        chat_message,
        id: MessageId::from(db_msg.id.as_str()),
        position: SmolStr::from(db_msg.position.as_str()),
        owner_id: AgentId::from(db_msg.agent_id.as_str()),
        created_at,
        batch: BatchId::from(batch),
        response_meta: None,
        block_refs: Vec::new(),
        attachments: Vec::new(),
    })
}

/// Group messages by batch_id, preserving position order within each batch.
///
/// Returns a `Vec<(BatchId, Vec<Message>)>` in the order the first message
/// of each batch appears. Messages with no batch_id are placed in a
/// synthetic "unknown" batch.
fn group_by_batch(messages: Vec<Message>) -> Vec<(BatchId, Vec<Message>)> {
    let mut batch_order: Vec<BatchId> = Vec::new();
    let mut groups: std::collections::HashMap<BatchId, Vec<Message>> =
        std::collections::HashMap::new();

    for msg in messages {
        let bid = msg.batch.clone();
        groups.entry(bid.clone()).or_default().push(msg);
        if !batch_order.contains(&bid) {
            batch_order.push(bid);
        }
    }

    batch_order
        .into_iter()
        .filter_map(|bid| {
            let msgs = groups.remove(&bid)?;
            Some((bid, msgs))
        })
        .collect()
}

/// Infer a `MessageOrigin` from a `pattern_db::models::BatchType`.
///
/// Inverse of `infer_batch_type` in `agent_loop.rs`. Since the DB doesn't
/// store the full author identity, we reconstruct a plausible default:
/// - `UserRequest` → Partner author (system sphere for simplicity).
/// - `SystemTrigger` → System/Wakeup.
/// - `Continuation` → System/ToolCall.
/// - `AgentToAgent` → Agent author with unknown agent_id.
fn infer_origin_from_batch_type(batch_type: BatchType) -> MessageOrigin {
    match batch_type {
        BatchType::UserRequest => MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        BatchType::SystemTrigger => MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        BatchType::Continuation => MessageOrigin::new(
            Author::System {
                reason: SystemReason::ToolCall,
            },
            Sphere::System,
        ),
        BatchType::AgentToAgent => MessageOrigin::new(
            Author::Agent(pattern_core::types::origin::AgentAuthor {
                agent_id: AgentId::from("unknown"),
            }),
            Sphere::Internal,
        ),
    }
}

/// Infer `StopReason` from the output messages of a reconstructed turn.
///
/// If any message has `ChatRole::Tool`, the turn ended with a tool call
/// (the tool_result message bundles with the prior assistant message).
/// Otherwise, it's a terminal `EndTurn`.
fn infer_stop_reason(output_msgs: &[Message]) -> StopReason {
    if output_msgs
        .iter()
        .any(|m| m.chat_message.role == ChatRole::Tool)
    {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    }
}

/// Extract `ToolCall` entries from an assistant message's content parts.
///
/// Walks `ContentPart::ToolCall` variants in the message's content and
/// clones them into a vec. Returns empty for non-assistant or text-only
/// messages.
fn infer_tool_calls(msg: &Message) -> Vec<ToolCall> {
    msg.chat_message
        .content
        .parts()
        .iter()
        .filter_map(|part| part.as_tool_call().cloned())
        .collect()
}

/// Run the turn-boundary detection algorithm on a batch of messages
/// (already ordered by position) to produce `TurnRecord`s.
///
/// Algorithm: walk messages in position order, accumulating input
/// (user/system) and output (assistant/tool) buffers. Boundary triggers:
/// - User/System role while already in output mode → close current turn,
///   start new input buffer.
/// - Assistant role while output buffer is non-empty → close current
///   turn as continuation (empty input), start new output.
/// - Tool role → always appends to output (bundles with prior assistant).
///
/// At end of batch: flush remaining buffers as one final `TurnRecord`.
///
/// ## Consecutive user-message merge
///
/// When two or more User/System messages appear back-to-back without any
/// intervening Assistant output, they are **merged into the same turn's
/// input buffer**. The first message is NOT closed off as a separate
/// `TurnRecord`; both messages become part of `TurnInput::messages` for
/// a single record.
///
/// This matches the canonical Anthropic multi-part user turn pattern
/// (text + image + text before the model responds) and avoids creating
/// phantom empty-output `TurnRecord`s for multi-part turns that arrive as
/// a single batch.
///
/// Example:
/// ```text
/// [User("text"), User("image"), Assistant("reply")]
///           └──── merged into one TurnRecord ────┘
/// ```
/// produces one `TurnRecord` with two input messages and one output message,
/// not three records.
fn build_turn_records_from_batch(
    batch_id: BatchId,
    msgs: Vec<Message>,
    batch_type: BatchType,
) -> Vec<TurnRecord> {
    let mut records = Vec::new();
    let mut current_input: Vec<Message> = Vec::new();
    let mut current_output: Vec<Message> = Vec::new();
    let mut in_output = false;

    let origin = infer_origin_from_batch_type(batch_type);

    for msg in msgs {
        match msg.chat_message.role {
            ChatRole::User | ChatRole::System => {
                if in_output {
                    // Close the previous turn.
                    records.push(flush_turn_record(
                        &batch_id,
                        &origin,
                        std::mem::take(&mut current_input),
                        std::mem::take(&mut current_output),
                    ));
                    in_output = false;
                }
                current_input.push(msg);
            }
            ChatRole::Assistant => {
                if in_output && !current_output.is_empty() {
                    // Close the previous turn; this assistant message starts
                    // a continuation turn (empty input).
                    records.push(flush_turn_record(
                        &batch_id,
                        &origin,
                        std::mem::take(&mut current_input),
                        std::mem::take(&mut current_output),
                    ));
                    // Continuation: input stays empty.
                }
                current_output.push(msg);
                in_output = true;
            }
            ChatRole::Tool => {
                // Tool results bundle with the prior assistant output.
                current_output.push(msg);
            }
        }
    }

    // Flush any remaining buffers.
    if !current_input.is_empty() || !current_output.is_empty() {
        records.push(flush_turn_record(
            &batch_id,
            &origin,
            current_input,
            current_output,
        ));
    }

    records
}

/// Build a synthetic `TurnRecord` from accumulated input/output buffers.
fn flush_turn_record(
    batch_id: &BatchId,
    origin: &MessageOrigin,
    input_msgs: Vec<Message>,
    output_msgs: Vec<Message>,
) -> TurnRecord {
    let turn_id = new_snowflake_id();
    let stop_reason = infer_stop_reason(&output_msgs);

    // Collect tool_calls from assistant messages in the output.
    let tool_calls: Vec<ToolCall> = output_msgs
        .iter()
        .filter(|m| m.chat_message.role == ChatRole::Assistant)
        .flat_map(infer_tool_calls)
        .collect();

    let completed_at = output_msgs
        .last()
        .map(|m| m.created_at)
        .unwrap_or_else(Timestamp::now);

    TurnRecord {
        turn_id: turn_id.clone(),
        input: TurnInput {
            turn_id: turn_id.clone(),
            batch_id: batch_id.clone(),
            origin: origin.clone(),
            messages: input_msgs,
        },
        output: TurnOutput {
            messages: output_msgs,
            block_writes: Vec::new(),
            tool_calls,
            stop_reason,
            usage: None,
            cache_metrics: Default::default(),
            completed_at,
        },
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
    use pattern_core::types::ids::{new_id, new_snowflake_id};
    use pattern_core::types::origin::{AgentAuthor, Author};
    use smol_str::SmolStr;

    fn make_turn_output(msg_count: usize, block_writes: Vec<BlockWrite>) -> TurnOutput {
        use pattern_core::types::turn::StopReason;
        TurnOutput {
            messages: (0..msg_count)
                .map(|i| Message {
                    chat_message: genai::chat::ChatMessage::user(format!("msg {i}")),
                    id: new_id(),
                    position: new_snowflake_id(),
                    owner_id: SmolStr::new("agent-a"),
                    created_at: Timestamp::now(),
                    batch: new_snowflake_id(),
                    response_meta: None,
                    block_refs: vec![],
                    attachments: vec![],
                })
                .collect(),
            block_writes,
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: None,
            cache_metrics: Default::default(),
            completed_at: Timestamp::now(),
        }
    }

    /// Build a minimal `TurnInput` for tests that don't care about
    /// the input shape — uses `continuation` so no message content
    /// is fabricated.
    fn make_turn_input_empty() -> TurnInput {
        use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
        TurnInput {
            turn_id: new_snowflake_id(),
            batch_id: new_snowflake_id(),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![],
        }
    }

    fn make_block_write(handle: &str) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_01"),
            block_type: pattern_core::types::memory_types::BlockType::Working,
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

        // Record with 0 input messages and 2 output messages.
        let output = make_turn_output(2, vec![]);
        hist.record(new_id(), make_turn_input_empty(), output);
        assert_eq!(hist.active_len(), 1);
        // active_messages interleaves input (0) + output (2) = 2.
        assert_eq!(hist.active_messages().count(), 2);
    }

    #[test]
    fn active_messages_interleaves_input_and_output() {
        // Verify that input messages appear BEFORE output messages for
        // each turn — the correct wire order for the composer.
        use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

        let batch = new_id();

        let make_msg = |text: &str, role: genai::chat::ChatRole| Message {
            chat_message: genai::chat::ChatMessage::new(role, text.to_string()),
            id: new_id(),
            position: new_snowflake_id(),
            owner_id: SmolStr::new("agent-a"),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };

        let user_msg = make_msg("user says hi", genai::chat::ChatRole::User);
        let assistant_msg = make_msg("agent replies", genai::chat::ChatRole::Assistant);

        let input = TurnInput {
            turn_id: new_snowflake_id(),
            batch_id: batch.clone(),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![user_msg],
        };

        let output = {
            use pattern_core::types::turn::StopReason;
            TurnOutput {
                messages: vec![assistant_msg],
                block_writes: vec![],
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: None,
                cache_metrics: Default::default(),
                completed_at: Timestamp::now(),
            }
        };

        let mut hist = TurnHistory::empty();
        hist.record(new_id(), input, output);

        let msgs: Vec<_> = hist.active_messages().collect();
        assert_eq!(msgs.len(), 2, "one input + one output message");
        assert_eq!(
            msgs[0].chat_message.role,
            genai::chat::ChatRole::User,
            "input message comes first"
        );
        assert_eq!(
            msgs[1].chat_message.role,
            genai::chat::ChatRole::Assistant,
            "output message comes second"
        );
    }

    #[test]
    fn estimated_tokens_accumulates_and_refresh_overwrites() {
        let mut hist = TurnHistory::empty();
        assert_eq!(hist.estimated_tokens(), 0);

        // Record turns with heuristic fallback.
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(1, vec![]),
        );
        let after_one = hist.estimated_tokens();
        assert!(after_one > 0, "heuristic should produce nonzero count");

        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(1, vec![]),
        );
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
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(0, vec![]),
        );
        assert!(hist.most_recent_block_writes().is_empty());

        // Second turn: has block writes.
        let writes = vec![make_block_write("notes"), make_block_write("tasks")];
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(0, writes),
        );
        assert_eq!(hist.most_recent_block_writes().len(), 2);
        assert_eq!(hist.most_recent_block_writes()[0].handle.as_str(), "notes");
    }

    #[test]
    fn take_oldest_removes_and_recomputes() {
        let mut hist = TurnHistory::empty();
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(3, vec![]),
        );
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(3, vec![]),
        );
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(3, vec![]),
        );
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
        hist.record(
            new_id(),
            make_turn_input_empty(),
            make_turn_output(1, vec![]),
        );

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

    // ---- Batch tracking tests ----

    fn make_turn_input_with_batch(batch_id: &str) -> TurnInput {
        use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
        TurnInput {
            turn_id: new_snowflake_id(),
            batch_id: SmolStr::new(batch_id),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![],
        }
    }

    #[test]
    fn batches_since_last_full_increments_on_new_batch() {
        let mut hist = TurnHistory::empty();
        assert_eq!(hist.batches_since_last_full(), 0);

        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-1"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.batches_since_last_full(), 1);

        // Same batch_id = no increment.
        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-1"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.batches_since_last_full(), 1);

        // New batch_id = increment.
        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-2"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.batches_since_last_full(), 2);
    }

    #[test]
    fn note_full_snapshot_resets_counter() {
        let mut hist = TurnHistory::empty();
        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-1"),
            make_turn_output(1, vec![]),
        );
        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-2"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.batches_since_last_full(), 2);

        hist.note_full_snapshot_emitted();
        assert_eq!(hist.batches_since_last_full(), 0);
        assert!(!hist.post_compaction_pending());
    }

    #[test]
    fn take_oldest_sets_post_compaction_pending() {
        let mut hist = TurnHistory::empty();
        assert!(!hist.post_compaction_pending());

        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-1"),
            make_turn_output(1, vec![]),
        );
        hist.take_oldest(1);
        assert!(hist.post_compaction_pending());
    }

    #[test]
    fn most_recent_batch_id_tracks_latest() {
        let mut hist = TurnHistory::empty();
        assert!(hist.most_recent_batch_id().is_none());

        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-1"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.most_recent_batch_id().unwrap().as_str(), "batch-1");

        hist.record(
            new_id(),
            make_turn_input_with_batch("batch-2"),
            make_turn_output(1, vec![]),
        );
        assert_eq!(hist.most_recent_batch_id().unwrap().as_str(), "batch-2");
    }

    // ---- build_turn_records_from_batch tests ----

    /// Helper: build a minimal [`Message`] with the given role.
    fn make_batch_msg(text: &str, role: ChatRole, batch_id: &SmolStr) -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(role, text.to_string()),
            id: new_id(),
            position: new_snowflake_id(),
            owner_id: SmolStr::new("agent-a"),
            created_at: jiff::Timestamp::now(),
            batch: batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        }
    }

    /// Two consecutive User messages with no intervening Assistant output must
    /// be merged into a single `TurnRecord`'s input buffer rather than
    /// producing two records with phantom empty outputs.
    ///
    /// Regression test for code-review finding #16 (document + test
    /// `build_turn_records_from_batch` consecutive-user-message merge).
    #[test]
    fn consecutive_user_messages_merge_into_one_turn() {
        let batch_id = SmolStr::new("batch-x");
        let msgs = vec![
            make_batch_msg("part one", ChatRole::User, &batch_id),
            make_batch_msg("part two", ChatRole::User, &batch_id),
            make_batch_msg("assistant reply", ChatRole::Assistant, &batch_id),
        ];

        let records = build_turn_records_from_batch(batch_id, msgs, BatchType::UserRequest);

        assert_eq!(
            records.len(),
            1,
            "two consecutive user messages + one assistant should produce exactly one TurnRecord"
        );
        assert_eq!(
            records[0].input.messages.len(),
            2,
            "both user messages should appear in the input buffer of the single record"
        );
        assert_eq!(
            records[0].output.messages.len(),
            1,
            "the assistant reply should be the sole output message"
        );
    }

    /// User, Assistant, User produces two turns: the boundary between the
    /// first assistant output and the second user message must trigger a
    /// new record.
    #[test]
    fn user_assistant_user_produces_two_turns() {
        let batch_id = SmolStr::new("batch-y");
        let msgs = vec![
            make_batch_msg("first question", ChatRole::User, &batch_id),
            make_batch_msg("first answer", ChatRole::Assistant, &batch_id),
            make_batch_msg("second question", ChatRole::User, &batch_id),
            make_batch_msg("second answer", ChatRole::Assistant, &batch_id),
        ];

        let records = build_turn_records_from_batch(batch_id, msgs, BatchType::UserRequest);

        assert_eq!(
            records.len(),
            2,
            "two question/answer pairs should produce two TurnRecords"
        );
        assert_eq!(
            records[0].input.messages.len(),
            1,
            "turn 1: one input message"
        );
        assert_eq!(
            records[0].output.messages.len(),
            1,
            "turn 1: one output message"
        );
        assert_eq!(
            records[1].input.messages.len(),
            1,
            "turn 2: one input message"
        );
        assert_eq!(
            records[1].output.messages.len(),
            1,
            "turn 2: one output message"
        );
    }

    /// Tool messages bundle with the preceding assistant output rather than
    /// starting a new turn.
    #[test]
    fn tool_result_bundles_with_assistant_output() {
        let batch_id = SmolStr::new("batch-z");
        let msgs = vec![
            make_batch_msg("user question", ChatRole::User, &batch_id),
            make_batch_msg("tool_use call", ChatRole::Assistant, &batch_id),
            make_batch_msg("tool result", ChatRole::Tool, &batch_id),
            make_batch_msg("final reply", ChatRole::Assistant, &batch_id),
        ];

        let records = build_turn_records_from_batch(batch_id, msgs, BatchType::UserRequest);

        // The tool result should bundle with the first assistant, then the
        // second assistant starts a continuation turn (empty input).
        assert_eq!(
            records.len(),
            2,
            "tool_use + tool_result + continuation assistant = two turns"
        );
        // Turn 1: user input + assistant(tool_use) + tool_result.
        assert_eq!(
            records[0].output.messages.len(),
            2,
            "first turn output: assistant + tool_result"
        );
        // Turn 2: continuation (empty input) + final reply.
        assert_eq!(
            records[1].input.messages.len(),
            0,
            "continuation turn has empty input"
        );
        assert_eq!(
            records[1].output.messages.len(),
            1,
            "continuation turn has one output message"
        );
    }
}
