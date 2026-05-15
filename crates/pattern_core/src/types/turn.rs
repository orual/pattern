//! Turn boundary types: `TurnInput`, `TurnOutput`, and `TurnId`.
//!
//! A *turn* is the unit of agent execution: one activation of the agent loop,
//! from receiving caller input through producing a final reply and recording
//! all side effects. Turns are checkpointable (Phase 3) and their outputs
//! drive attachment rendering via the compose pipeline.
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
//!
//! # Multi-turn tool-use round-trips
//!
//! `TurnHistory` (in `pattern_runtime`) stores both the input and output
//! for each turn as a `TurnRecord`, so the full conversational round-trip
//! is preserved: user message → assistant reply → tool_result. On a
//! tool-use turn, `orchestrate` synthesises a `ChatRole::Tool` message
//! from the dispatched results and appends it to `TurnOutput.messages`.
//! Continuation turns are built via [`TurnInput::continuation`] with
//! empty `messages`; the prior turn's tool_result message lives in
//! history and is replayed by the composer.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::types::block::BlockWrite;
use crate::types::ids::{BatchId, new_snowflake_id};
use crate::types::message::Message;
use crate::types::origin::MessageOrigin;
use crate::types::provider::{ToolCall, ToolOutcome, ToolResult};

// `TurnId` is defined in `types::ids` as a `SmolStr` type alias. Mint fresh
// turn ids via `pattern_core::types::ids::new_id()`.
pub use crate::types::ids::TurnId;

/// Input to a single **wire-level** agent turn.
///
/// A "wire turn" corresponds to one provider API call. One user-visible
/// exchange (one `Session::step` invocation) produces N wire turns — the
/// first wire turn's input carries the caller's messages, and each
/// subsequent wire turn is a continuation (via [`TurnInput::continuation`])
/// with empty `messages`. The prior turn's assistant reply and tool_result
/// message already live in `TurnHistory` (in `pattern_runtime`) and are
/// replayed by the composer's Segment 2 pass; no new messages are needed
/// on the continuation input.
///
/// All wire turns within a single `Session::step` share the same
/// [`BatchId`]. Each gets a freshly-minted [`TurnId`] at construction.
///
/// # Examples
///
/// ```
/// use pattern_core::types::ids::{new_snowflake_id, BatchId};
/// use pattern_core::types::turn::TurnInput;
/// use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
///
/// // Fresh-batch start: turn_id == batch_id (first turn IS the batch).
/// let id = new_snowflake_id();
/// let input = TurnInput {
///     turn_id: id.clone(),
///     batch_id: BatchId::from(id),
///     origin: MessageOrigin::new(
///         Author::System { reason: SystemReason::Wakeup },
///         Sphere::System,
///     ),
///     messages: vec![],
/// };
/// // Fresh-batch: turn_id and batch_id are the same snowflake.
/// assert_eq!(input.turn_id, input.batch_id);
/// assert!(input.messages.is_empty());
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
    /// Build a continuation input (zero new user messages).
    ///
    /// Used on agent-loop iterations after a tool_use turn. The prior turn's
    /// assistant message and synthesized tool_result message have already been
    /// recorded to [`TurnHistory`] by `drive_step` via `hist.record`, so the
    /// composer's Segment 2 pass replays them from history — this continuation
    /// input contributes no fresh messages of its own.
    ///
    /// Preserves `batch_id` — all wire turns in one step share a batch.
    /// Mints a fresh `turn_id`.
    ///
    /// Origin: System-authored (pattern synthesised this as a follow-up),
    /// System sphere.
    ///
    /// # Examples
    ///
    /// ```
    /// use pattern_core::types::ids::{new_snowflake_id, AgentId, BatchId};
    /// use pattern_core::types::turn::TurnInput;
    ///
    /// let batch = BatchId::from(new_snowflake_id());
    /// let next = TurnInput::continuation(batch.clone(), AgentId::from("agent-a"));
    /// assert_eq!(next.batch_id, batch);
    /// assert!(next.messages.is_empty(), "continuation carries no fresh messages");
    /// ```
    ///
    /// [`TurnHistory`]: crate::memory
    pub fn continuation(batch_id: BatchId, owner_id: crate::types::ids::AgentId) -> Self {
        let _ = owner_id; // stored in the origin; field unused at construction
        let origin = MessageOrigin::new(
            crate::types::origin::Author::System {
                reason: crate::types::origin::SystemReason::ToolCall,
            },
            crate::types::origin::Sphere::System,
        );

        Self {
            turn_id: new_snowflake_id(),
            batch_id,
            origin,
            messages: Vec::new(), // empty — continuation content is in history
        }
    }
}

/// Output produced by a completed **wire-level** agent turn.
///
/// Collects everything one provider-call activation produced: reply
/// messages, memory block writes, tool_use blocks the LLM requested,
/// the provider's `stop_reason`, token usage if reported, cache metrics,
/// and the wall-clock completion time.
///
/// # Stored message sequence
///
/// On a tool-use turn, `messages` carries the full round-trip in order:
/// 1. The assistant message (with tool_use content parts).
/// 2. A `ChatRole::Tool` message carrying all tool_result blocks for
///    this turn, synthesised by `orchestrate` after dispatching the
///    tool calls.
///
/// On an `EndTurn` turn, `messages` contains only the assistant message.
/// Together with the turn's `TurnInput.messages` (stored alongside by
/// `TurnHistory`), this gives the composer everything it needs to replay
/// a complete conversational round-trip.
///
/// # Invariants
///
/// - `tool_calls` is non-empty IFF `stop_reason == StopReason::ToolUse`.
/// - When `stop_reason == ToolUse`, `messages` contains both an assistant
///   message and a tool_result message (the latter holds one
///   `ContentPart::ToolResponse` per dispatched call, 1:1 with `tool_calls`).
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
    ///
    /// On a tool-use turn this is `[assistant_msg, tool_result_msg]` in order;
    /// on an `EndTurn` turn it is `[assistant_msg]`. The composer's Segment 2
    /// pass replays these from `TurnHistory` on subsequent wire turns.
    pub messages: Vec<Message>,
    /// Memory block writes that occurred during this turn, in order.
    pub block_writes: Vec<BlockWrite>,
    /// Tool calls the LLM requested during this wire turn. Non-empty only
    /// when `stop_reason == ToolUse`. Documents what the model requested;
    /// the corresponding results are inlined into `messages` as the
    /// tool_result message.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
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

impl TurnOutput {
    /// Reconstruct [`ToolResult`] views from the inlined tool_result
    /// message in [`Self::messages`].
    ///
    /// Walks `messages`, finds the `ChatRole::Tool` message (if any),
    /// and collects one `ToolResult` per `ContentPart::ToolResponse`
    /// part. Callers that need direct `ToolResult` access without
    /// re-walking messages themselves can use this convenience accessor.
    ///
    /// Returns `ToolOutcome::Success(content)` for every result — the
    /// error/success distinction is not round-tripped through the message
    /// representation at this layer. Callers that need to distinguish
    /// error outcomes should retain the original `Vec<ToolResult>` before
    /// it is inlined (e.g. from `orchestrate`'s local variable).
    ///
    /// Returns an empty `Vec` when `stop_reason != ToolUse`.
    pub fn tool_results(&self) -> Vec<ToolResult> {
        use genai::chat::{ChatRole, ContentPart};
        self.messages
            .iter()
            .filter(|m| m.chat_message.role == ChatRole::Tool)
            .flat_map(|m| m.chat_message.content.parts().iter())
            .filter_map(|part| {
                if let ContentPart::ToolResponse(tr) = part {
                    // Direct vec move — Vec<ContentPart> preserves multi-modal fidelity
                    // from wire ToolResponse through ToolOutcome and back.
                    Some(ToolResult {
                        call_id: tr.call_id.clone(),
                        outcome: ToolOutcome::Success(tr.content.clone()),
                    })
                } else {
                    None
                }
            })
            .collect()
    }
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
/// the caller's input, each subsequent wire turn is a continuation
/// (via [`TurnInput::continuation`]) with empty messages. This
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

    /// All `ToolCall` / `ToolResult` pairs across all wire turns, in order.
    ///
    /// Returns owned pairs. The `call_id` fields match per [`TurnOutput`]'s
    /// invariant. `ToolResult.outcome` is reconstructed from the inlined
    /// tool_result message (always `Success` — see [`TurnOutput::tool_results`]).
    pub fn all_tool_exchanges(&self) -> Vec<(ToolCall, ToolResult)> {
        self.turns
            .iter()
            .flat_map(|t| {
                let results = t.tool_results();
                t.tool_calls.iter().cloned().zip(results)
            })
            .collect()
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
        use crate::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};

        fn make_msg(text: &str, batch: &BatchId) -> Message {
            Message {
                chat_message: genai::chat::ChatMessage::new(
                    genai::chat::ChatRole::Assistant,
                    text.to_string(),
                ),
                id: MessageId::from(new_id()),
                position: new_snowflake_id(),
                owner_id: AgentId::from("agent-a"),
                created_at: Timestamp::now(),
                batch: batch.clone(),
                response_meta: None,
                block_refs: vec![],
                attachments: vec![],
            }
        }

        let batch = BatchId::from(new_snowflake_id());
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
        use crate::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};

        let batch = BatchId::from(new_snowflake_id());
        let make_assistant = |text: &str| Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::Assistant,
                text.to_string(),
            ),
            id: MessageId::from(new_id()),
            position: new_snowflake_id(),
            owner_id: AgentId::from("agent-a"),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };
        let make_tool = || Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::Tool,
                "tool noise".to_string(),
            ),
            id: MessageId::from(new_id()),
            position: new_snowflake_id(),
            owner_id: AgentId::from("agent-a"),
            created_at: Timestamp::now(),
            batch: batch.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
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
