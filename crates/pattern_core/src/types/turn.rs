//! Turn boundary types: `TurnInput`, `TurnOutput`, and `TurnId`.
//!
//! A *turn* is the unit of agent execution: one activation of the agent loop,
//! from receiving caller input through producing a final reply and recording
//! all side effects. Turns are checkpointable (Phase 3) and their outputs
//! drive pseudo-message emission (Phase 5).
//!
//! # Turn contract
//!
//! Every turn begins with a [`TurnInput`] that carries the caller identity,
//! the incoming messages, and a stable [`TurnId`] assigned before the turn
//! starts. When the agent loop completes, it produces a [`TurnOutput`] that
//! collects all reply messages, the memory block writes that occurred, token
//! usage if available, and the completion timestamp.
//!
//! The [`TurnId`] serves as a checkpoint key: `block_changes_since(turn)` can
//! reconstruct exactly which blocks changed during that turn.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::types::block::BlockWrite;
use crate::types::ids::{BatchId, new_id};
use crate::types::message::Message;
use crate::types::origin::MessageOrigin;
use crate::types::provider::{ToolCall, ToolResult};

// `TurnId` is defined in `types::ids` as a `SmolStr` type alias. Mint fresh
// turn ids via `pattern_core::types::ids::new_id()`.
pub use crate::types::ids::TurnId;

/// Input to a single **wire-level** agent turn.
///
/// A "wire turn" corresponds to one provider API call. One user-visible
/// exchange (one `Session::step` invocation) produces N wire turns — the
/// first wire turn's input carries the caller's messages, and each
/// subsequent wire turn's input carries the prior turn's tool_results
/// (via [`TurnInput::from_tool_results`]).
///
/// All wire turns within a single `Session::step` share the same
/// [`BatchId`]. Each gets a freshly-minted [`TurnId`] at construction.
///
/// # Examples
///
/// ```
/// use pattern_core::types::ids::{new_id, BatchId};
/// use pattern_core::types::turn::TurnInput;
/// use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
///
/// let input = TurnInput {
///     turn_id: new_id(),
///     batch_id: BatchId::from(new_id()),
///     origin: MessageOrigin::new(
///         Author::System { reason: SystemReason::Wakeup },
///         Sphere::System,
///     ),
///     messages: vec![],
/// };
/// assert_eq!(input.turn_id.len(), 32);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInput {
    /// Stable identifier assigned before the wire turn begins.
    pub turn_id: TurnId,
    /// Batch identifier stable across all wire turns in one
    /// `Session::step` call. Distinct `Session::step` calls mint
    /// fresh batches.
    pub batch_id: BatchId,
    /// Provenance of the messages delivered this turn — who authored them
    /// and into what visibility sphere.
    pub origin: MessageOrigin,
    /// Messages delivered to the agent for this activation.
    pub messages: Vec<Message>,
}

impl TurnInput {
    /// Build the next wire turn's input from a prior wire turn's
    /// tool_results.
    ///
    /// Used by the agent-loop driver in `TidepoolSession::step` to chain
    /// tool_use cycles. Each `ToolResult` becomes a `ToolResponse`
    /// content part on a single `ChatRole::Tool` message; the wire
    /// format puts all tool_result blocks into one user-role message
    /// per Anthropic's tool-use protocol.
    ///
    /// Preserves `batch_id` — all wire turns in one step share a batch.
    /// Mints a fresh `turn_id`.
    ///
    /// # Panics
    ///
    /// Panics if `prior.tool_results` is empty. Callers should only
    /// invoke this when the prior turn's `stop_reason == ToolUse` and
    /// there is at least one tool_result to deliver.
    ///
    /// # Examples
    ///
    /// ```
    /// use jiff::Timestamp;
    /// use pattern_core::types::ids::{new_id, AgentId, BatchId};
    /// use pattern_core::types::provider::{ToolOutcome, ToolResult};
    /// use pattern_core::types::turn::{StopReason, TurnInput, TurnOutput};
    ///
    /// let batch = BatchId::from(new_id());
    /// let prior = TurnOutput {
    ///     messages: vec![],
    ///     block_writes: vec![],
    ///     tool_calls: vec![],
    ///     tool_results: vec![ToolResult {
    ///         call_id: "toolu_01".into(),
    ///         outcome: ToolOutcome::Success(serde_json::json!({"ok": true})),
    ///     }],
    ///     stop_reason: StopReason::ToolUse,
    ///     usage: None,
    ///     cache_metrics: Default::default(),
    ///     completed_at: Timestamp::now(),
    /// };
    /// let next = TurnInput::from_tool_results(
    ///     &prior,
    ///     batch.clone(),
    ///     AgentId::from("agent-a"),
    /// );
    /// assert_eq!(next.batch_id, batch);
    /// assert_eq!(next.messages.len(), 1);
    /// ```
    pub fn from_tool_results(
        prior: &TurnOutput,
        batch_id: BatchId,
        owner_id: crate::types::ids::AgentId,
    ) -> Self {
        assert!(
            !prior.tool_results.is_empty(),
            "from_tool_results called with no tool_results — \
             the caller should check stop_reason first"
        );

        // Build one ChatMessage::Tool carrying all tool_result blocks.
        // genai's ChatRole::Tool + ToolResponse content parts maps 1:1
        // to Anthropic's user-role message with tool_result content
        // blocks (the provider adapter handles the role translation).
        use genai::chat::{ChatMessage, ChatRole, ContentPart, MessageContent};
        let parts: Vec<ContentPart> = prior
            .tool_results
            .iter()
            .map(|r| ContentPart::from(r.to_tool_response()))
            .collect();

        let chat_message = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(parts),
            options: Default::default(),
        };

        let now = Timestamp::now();
        let message = Message {
            chat_message,
            id: crate::types::ids::MessageId::from(new_id()),
            owner_id,
            created_at: now,
            batch: batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
        };

        // Origin for a tool-result turn: system-authored (pattern
        // delivered the results, not a human), system-visibility.
        let origin = MessageOrigin::new(
            crate::types::origin::Author::System {
                reason: crate::types::origin::SystemReason::ToolCall,
            },
            crate::types::origin::Sphere::System,
        );

        Self {
            turn_id: new_id(),
            batch_id,
            origin,
            messages: vec![message],
        }
    }
}

/// Output produced by a completed **wire-level** agent turn.
///
/// Collects everything one provider-call activation produced: reply
/// messages, memory block writes, tool_use blocks the LLM requested,
/// tool_results the agent loop executed, the provider's `stop_reason`,
/// token usage if reported, cache metrics, and the wall-clock
/// completion time.
///
/// # Invariants
///
/// - `tool_calls.len() == tool_results.len()` and both are non-empty
///   IFF `stop_reason == StopReason::ToolUse`. When the stream ended
///   for any other reason, both vectors are empty.
/// - `tool_calls[i]` and `tool_results[i]` share the same `call_id`
///   (paired 1:1 in the order the provider emitted the tool_use
///   blocks).
/// - `block_writes` is the authoritative record of memory mutations
///   within this wire turn, drained from the adapter's pending buffer
///   at turn close.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::turn::{StopReason, TurnOutput};
///
/// let output = TurnOutput {
///     messages: vec![],
///     block_writes: vec![],
///     tool_calls: vec![],
///     tool_results: vec![],
///     stop_reason: StopReason::EndTurn,
///     usage: None,
///     cache_metrics: Default::default(),
///     completed_at: Timestamp::now(),
/// };
/// assert!(output.block_writes.is_empty());
/// assert!(output.stop_reason.is_terminal());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutput {
    /// Reply messages produced during this turn (assistant + tool responses).
    pub messages: Vec<Message>,
    /// Memory block writes that occurred during this turn, in order.
    pub block_writes: Vec<BlockWrite>,
    /// Tool calls the LLM requested during this wire turn. Paired 1:1
    /// by index (and `call_id`) with `tool_results`. Empty unless
    /// `stop_reason == ToolUse`.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Results from executing `tool_calls`. Paired 1:1 by index (and
    /// `call_id`) with `tool_calls`. Empty unless `stop_reason ==
    /// ToolUse`. Each result's outcome distinguishes success
    /// (JSON payload) from error (string message).
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
    /// Why this wire turn's stream terminated. Drives the agent-loop
    /// driver's decision to loop (ToolUse) or return (everything
    /// else).
    #[serde(default = "default_stop_reason")]
    pub stop_reason: StopReason,
    /// Token usage reported by the provider, if available.
    pub usage: Option<genai::chat::Usage>,
    /// Provider cache metrics for this turn (empty in Phase 2).
    #[serde(default)]
    pub cache_metrics: TurnCacheMetrics,
    /// Wall-clock time at which the turn completed.
    pub completed_at: Timestamp,
}

/// Default stop reason for deserialisation — used when reading
/// historic `TurnOutput` records that pre-date the field addition.
/// `EndTurn` is the conservative choice: it means "terminal" so
/// replay won't try to issue a follow-up turn from an old record.
fn default_stop_reason() -> StopReason {
    StopReason::EndTurn
}

/// Provider-reported cache metrics for a single wire turn.
///
/// Populated from the `usage` field of the provider's `StreamEnd` event.
/// For Anthropic, the three token buckets correspond directly to the
/// fields on the response's `usage` object:
///
/// - `fresh_input_tokens` ← `input_tokens` (tokens charged at the base rate)
/// - `cache_read_input_tokens` ← `cache_read_input_tokens` (billed at 0.1×)
/// - `cache_creation_input_tokens` ← `cache_creation_input_tokens` (billed at
///   1.25× for 5-minute TTL or 2× for 1-hour TTL)
///
/// The struct uses `#[non_exhaustive]` so that future fields (e.g.
/// per-TTL creation breakdown) can be added without breaking exhaustive
/// construction call sites.
///
/// # Examples
///
/// ```
/// use pattern_core::types::turn::TurnCacheMetrics;
///
/// let m = TurnCacheMetrics::new(100, 900, 0);
/// assert!((m.hit_ratio() - 0.9).abs() < 1e-9);
/// assert_eq!(m.total_input_tokens(), 1000);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnCacheMetrics {
    /// Tokens charged at the fresh-input rate (no cache involvement).
    pub fresh_input_tokens: u64,
    /// Tokens read from existing cache entries. Billed at 0.1× base.
    pub cache_read_input_tokens: u64,
    /// Tokens committed to new cache entries this turn. Billed at
    /// 1.25× (5-minute TTL) or 2× (1-hour TTL).
    pub cache_creation_input_tokens: u64,
}

impl TurnCacheMetrics {
    /// Construct from the three Anthropic billing buckets.
    ///
    /// This is the canonical constructor — it is required because the struct
    /// is `#[non_exhaustive]`, preventing literal construction outside of
    /// `pattern_core`.
    pub fn new(
        fresh_input_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    ) -> Self {
        Self {
            fresh_input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        }
    }

    /// Cache-hit ratio: `cache_read / (cache_read + fresh_input)`.
    ///
    /// Returns `0.0` when no input tokens were counted (avoids
    /// division by zero). Cache-creation tokens are excluded from the
    /// denominator because they represent new cache writes, not
    /// re-use of existing content.
    pub fn hit_ratio(&self) -> f64 {
        let denominator = self.cache_read_input_tokens + self.fresh_input_tokens;
        if denominator == 0 {
            0.0
        } else {
            self.cache_read_input_tokens as f64 / denominator as f64
        }
    }

    /// Total input tokens: `fresh + cache_read + cache_creation`.
    ///
    /// This is the sum over all three billing buckets.
    pub fn total_input_tokens(&self) -> u64 {
        self.fresh_input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }
}

/// Why a single **wire-level** turn ended.
///
/// One provider call produces one [`TurnOutput`] that carries one
/// `StopReason`. The Phase 5 Task 20 agent loop uses this to decide
/// whether to issue a follow-up wire turn with `tool_result` blocks
/// (when `ToolUse`) or terminate the user-visible exchange (everything
/// else).
///
/// Values correspond to Anthropic's `stop_reason` field on streamed
/// responses; other providers map their terminal conditions onto the
/// same vocabulary. `PauseTurn` is server-tool-specific (Anthropic
/// emits it when an internal server-tool loop hits its iteration cap)
/// and is not expected on Pattern's Phase 5 client-tool path, but is
/// declared for API stability.
///
/// # Examples
///
/// ```
/// use pattern_core::types::turn::StopReason;
///
/// assert!(StopReason::EndTurn.is_terminal());
/// assert!(!StopReason::ToolUse.is_terminal());
/// assert!(StopReason::MaxTokens.is_terminal());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Agent emitted a terminal assistant message with no tool calls;
    /// the user-visible exchange is complete.
    EndTurn,
    /// Agent requested one or more tool calls; the driver must execute
    /// them and issue a follow-up wire turn with the results.
    ToolUse,
    /// Response hit the configured `max_tokens` budget before reaching
    /// a natural stopping point. Callers typically surface this to the
    /// operator rather than looping.
    MaxTokens,
    /// Response matched a caller-provided stop sequence.
    StopSequence,
    /// Model refused the request (safety layer). Treated as terminal
    /// by Phase 5 — the driver stops looping and surfaces the refusal.
    Refusal,
    /// Server-side tool loop hit its internal iteration cap; the
    /// conversation can be resumed by re-sending the same request.
    /// Not expected on client-tool paths.
    PauseTurn,
}

impl StopReason {
    /// `true` when this reason ends the user-visible exchange — i.e.
    /// anything EXCEPT `ToolUse`. The agent-loop driver checks this to
    /// decide whether to issue a follow-up wire turn.
    pub fn is_terminal(self) -> bool {
        !matches!(self, StopReason::ToolUse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_terminal_tool_use_is_only_non_terminal() {
        assert!(StopReason::EndTurn.is_terminal());
        assert!(!StopReason::ToolUse.is_terminal());
        assert!(StopReason::MaxTokens.is_terminal());
        assert!(StopReason::StopSequence.is_terminal());
        assert!(StopReason::Refusal.is_terminal());
        assert!(StopReason::PauseTurn.is_terminal());
    }

    #[test]
    fn stop_reason_serde_snake_case() {
        let j = serde_json::to_string(&StopReason::EndTurn).unwrap();
        assert_eq!(j, r#""end_turn""#);
        let j = serde_json::to_string(&StopReason::ToolUse).unwrap();
        assert_eq!(j, r#""tool_use""#);
        let j = serde_json::to_string(&StopReason::PauseTurn).unwrap();
        assert_eq!(j, r#""pause_turn""#);

        let r: StopReason = serde_json::from_str(r#""end_turn""#).unwrap();
        assert_eq!(r, StopReason::EndTurn);
        let r: StopReason = serde_json::from_str(r#""tool_use""#).unwrap();
        assert_eq!(r, StopReason::ToolUse);
    }
}

#[cfg(test)]
mod cache_metrics_tests {
    use super::*;

    #[test]
    fn hit_ratio_zero_when_no_tokens() {
        let m = TurnCacheMetrics::default();
        assert_eq!(m.hit_ratio(), 0.0);
        assert_eq!(m.total_input_tokens(), 0);
    }

    #[test]
    fn hit_ratio_all_cached_returns_one() {
        let m = TurnCacheMetrics {
            fresh_input_tokens: 0,
            cache_read_input_tokens: 1000,
            cache_creation_input_tokens: 0,
        };
        assert!((m.hit_ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn hit_ratio_all_fresh_returns_zero() {
        let m = TurnCacheMetrics {
            fresh_input_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert_eq!(m.hit_ratio(), 0.0);
    }

    #[test]
    fn hit_ratio_partial_cache() {
        // 900 cache_read + 100 fresh → 0.9 hit ratio.
        let m = TurnCacheMetrics {
            fresh_input_tokens: 100,
            cache_read_input_tokens: 900,
            cache_creation_input_tokens: 0,
        };
        assert!((m.hit_ratio() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn hit_ratio_excludes_cache_creation_from_denominator() {
        // cache_creation tokens represent write cost, not cache re-use.
        // Denominator is cache_read + fresh only.
        let m = TurnCacheMetrics {
            fresh_input_tokens: 100,
            cache_read_input_tokens: 900,
            cache_creation_input_tokens: 5000,
        };
        assert!((m.hit_ratio() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn total_input_tokens_sums_all_buckets() {
        let m = TurnCacheMetrics {
            fresh_input_tokens: 100,
            cache_read_input_tokens: 900,
            cache_creation_input_tokens: 200,
        };
        assert_eq!(m.total_input_tokens(), 1200);
    }

    #[test]
    fn serde_round_trips() {
        let m = TurnCacheMetrics {
            fresh_input_tokens: 42,
            cache_read_input_tokens: 100,
            cache_creation_input_tokens: 25,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let m2: TurnCacheMetrics = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m2.fresh_input_tokens, 42);
        assert_eq!(m2.cache_read_input_tokens, 100);
        assert_eq!(m2.cache_creation_input_tokens, 25);
    }
}

/// Aggregated output of one user-visible exchange — the return type
/// of [`crate::traits::Session::step`].
///
/// One `Session::step` call drives N wire turns: the first carries
/// the caller's input, each subsequent wire turn carries the prior
/// turn's tool_results (via [`TurnInput::from_tool_results`]). This
/// struct collects every wire turn's [`TurnOutput`] in order plus
/// convenience accessors + aggregates.
///
/// # Invariants
///
/// - `turns` is non-empty (every `step` call produces at least one
///   wire turn, even if it errors mid-stream).
/// - `final_stop_reason == turns.last().stop_reason`.
/// - All turns share the same `batch_id` (from the caller's input).
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::turn::{StepReply, StopReason, TurnOutput};
///
/// let turn = TurnOutput {
///     messages: vec![],
///     block_writes: vec![],
///     tool_calls: vec![],
///     tool_results: vec![],
///     stop_reason: StopReason::EndTurn,
///     usage: None,
///     cache_metrics: Default::default(),
///     completed_at: Timestamp::now(),
/// };
/// let reply = StepReply {
///     turns: vec![turn],
///     final_stop_reason: StopReason::EndTurn,
///     total_usage: None,
/// };
/// assert_eq!(reply.turn_count(), 1);
/// assert!(reply.final_stop_reason.is_terminal());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReply {
    /// Individual wire-turn outputs in the order they were produced.
    pub turns: Vec<TurnOutput>,
    /// Why the loop exited — always equal to `turns.last().stop_reason`.
    pub final_stop_reason: StopReason,
    /// Summed token usage across all wire turns. `None` when no turn
    /// reported usage (e.g. every call erred before the `End` event).
    /// Individual turns' usage is still available on
    /// `turns[i].usage`.
    pub total_usage: Option<genai::chat::Usage>,
}

impl StepReply {
    /// Number of wire turns produced. Always at least 1 for a
    /// non-errored step.
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Iterator over every `Message` produced across all wire turns,
    /// in order. Convenience for callers that don't care about turn
    /// boundaries.
    pub fn all_messages(&self) -> impl Iterator<Item = &Message> {
        self.turns.iter().flat_map(|t| t.messages.iter())
    }

    /// Iterator over every `BlockWrite` across all wire turns, in
    /// order. Convenience for callers that want the aggregate memory
    /// mutation record for the exchange.
    pub fn all_block_writes(&self) -> impl Iterator<Item = &BlockWrite> {
        self.turns.iter().flat_map(|t| t.block_writes.iter())
    }

    /// Iterator over every `ToolCall` / `ToolResult` pair across all
    /// wire turns, in order. The pair always matches by `call_id` per
    /// [`TurnOutput`]'s invariant.
    pub fn all_tool_exchanges(&self) -> impl Iterator<Item = (&ToolCall, &ToolResult)> {
        self.turns
            .iter()
            .flat_map(|t| t.tool_calls.iter().zip(t.tool_results.iter()))
    }

    /// Concatenated text content of assistant messages across every
    /// wire turn. Returns `None` if no assistant messages were
    /// produced.
    ///
    /// Useful for single-line CLIs that just want to print what the
    /// agent said across the whole exchange. More nuanced UIs should
    /// iterate [`Self::all_messages`] and render each turn
    /// individually.
    pub fn final_text(&self) -> Option<String> {
        let text: String = self
            .all_messages()
            .filter(|m| m.chat_message.role == genai::chat::ChatRole::Assistant)
            .filter_map(|m| m.chat_message.content.joined_texts())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() { None } else { Some(text) }
    }
}

#[cfg(test)]
mod step_reply_tests {
    use super::*;

    fn make_turn(stop: StopReason) -> TurnOutput {
        TurnOutput {
            messages: vec![],
            block_writes: vec![],
            tool_calls: vec![],
            tool_results: vec![],
            stop_reason: stop,
            usage: None,
            cache_metrics: Default::default(),
            completed_at: Timestamp::now(),
        }
    }

    #[test]
    fn turn_count_single_turn() {
        let reply = StepReply {
            turns: vec![make_turn(StopReason::EndTurn)],
            final_stop_reason: StopReason::EndTurn,
            total_usage: None,
        };
        assert_eq!(reply.turn_count(), 1);
    }

    #[test]
    fn turn_count_multi_turn() {
        let reply = StepReply {
            turns: vec![
                make_turn(StopReason::ToolUse),
                make_turn(StopReason::ToolUse),
                make_turn(StopReason::EndTurn),
            ],
            final_stop_reason: StopReason::EndTurn,
            total_usage: None,
        };
        assert_eq!(reply.turn_count(), 3);
    }

    #[test]
    fn all_messages_iterates_in_order_across_turns() {
        use crate::types::ids::{AgentId, BatchId, MessageId, new_id};

        fn make_msg(text: &str, batch: &BatchId) -> Message {
            Message {
                chat_message: genai::chat::ChatMessage::new(
                    genai::chat::ChatRole::Assistant,
                    text.to_string(),
                ),
                id: MessageId::from(new_id()),
                owner_id: AgentId::from("agent-a"),
                created_at: Timestamp::now(),
                batch: batch.clone(),
                response_meta: None,
                block_refs: vec![],
            }
        }

        let batch = BatchId::from(new_id());
        let mut t1 = make_turn(StopReason::ToolUse);
        t1.messages.push(make_msg("first", &batch));
        let mut t2 = make_turn(StopReason::EndTurn);
        t2.messages.push(make_msg("second", &batch));
        t2.messages.push(make_msg("third", &batch));

        let reply = StepReply {
            turns: vec![t1, t2],
            final_stop_reason: StopReason::EndTurn,
            total_usage: None,
        };

        let texts: Vec<String> = reply
            .all_messages()
            .filter_map(|m| m.chat_message.content.joined_texts())
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn final_text_joins_assistant_messages() {
        use crate::types::ids::{AgentId, BatchId, MessageId, new_id};

        let batch = BatchId::from(new_id());
        let make_assistant = |text: &str| Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::Assistant,
                text.to_string(),
            ),
            id: MessageId::from(new_id()),
            owner_id: AgentId::from("agent-a"),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
        };
        let make_tool = || Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::Tool,
                "tool noise".to_string(),
            ),
            id: MessageId::from(new_id()),
            owner_id: AgentId::from("agent-a"),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
        };

        let mut t1 = make_turn(StopReason::ToolUse);
        t1.messages.push(make_assistant("hello"));
        t1.messages.push(make_tool()); // should NOT appear in final_text
        let mut t2 = make_turn(StopReason::EndTurn);
        t2.messages.push(make_assistant("world"));

        let reply = StepReply {
            turns: vec![t1, t2],
            final_stop_reason: StopReason::EndTurn,
            total_usage: None,
        };

        let text = reply.final_text().unwrap();
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn final_text_none_when_no_assistant() {
        let reply = StepReply {
            turns: vec![make_turn(StopReason::EndTurn)],
            final_stop_reason: StopReason::EndTurn,
            total_usage: None,
        };
        assert_eq!(reply.final_text(), None);
    }
}
