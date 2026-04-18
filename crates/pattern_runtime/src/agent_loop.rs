//! Agent-loop orchestrator — executes **one wire turn** end-to-end.
//!
//! A "wire turn" corresponds to one `ProviderClient::complete` call. One
//! user-visible exchange ([`pattern_core::Session::step`]) is driven by
//! [`TidepoolSession::step`] as a loop over multiple wire turns,
//! chained by [`TurnInput::from_tool_results`] when `stop_reason ==
//! ToolUse`. This module implements the inner single-turn primitive.
//!
//! # Responsibilities
//!
//! 1. Build a [`CompletionRequest`] from the turn's `TurnInput` +
//!    [`crate::sdk::CODE_TOOL`] (full composer integration — segments
//!    1 / 2 / 3 — is wired in a follow-up change; this cut passes
//!    input messages through and injects the `code` tool).
//! 2. Stream the provider response, emitting [`TurnEvent`]s to the
//!    session's [`TurnSink`] as events arrive: `Text` for LLM
//!    response chunks, `Thinking` for reasoning chunks, `ToolCall`
//!    when tool_use blocks complete, `ToolResult` after eval settles,
//!    `Stop` when the wire turn closes.
//! 3. Dispatch each captured tool_use to the provided [`EvalDispatcher`]
//!    after stream close, collect outcomes, and pair them by
//!    `call_id` into [`ToolResult`]s.
//! 4. Drain the memory adapter's pending [`BlockWrite`]s and assemble
//!    a [`TurnOutput`] with: `messages` (including a reconstructed
//!    assistant message), `block_writes`, `tool_calls`, `tool_results`,
//!    `stop_reason`, `usage`, and a `completed_at` timestamp.
//!
//! # Thinking preservation
//!
//! Reasoning chunks stream as `ChatStreamEvent::ReasoningChunk` and
//! are accumulated into `captured_reasoning_content` at stream end.
//! Anthropic's `thinking` wire blocks also carry a signature that
//! must accompany the reasoning content on echo-back; genai's fork
//! models this via `ContentPart::ThoughtSignature` on the assembled
//! assistant message. Until the Anthropic adapter patch lands
//! (tracked in phase 6), signatures don't round-trip on the
//! outbound wire — so Extended Thinking with tool_use degrades
//! (model strips prior thinking from context), but the sink still
//! sees `TurnEvent::Thinking` chunks for UI purposes.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use jiff::Timestamp;

use pattern_core::error::RuntimeError;
use pattern_core::memory::StructuredDocument;
use pattern_core::traits::TurnEvent;
use pattern_core::types::ids::{AgentId, MessageId, new_id};
use pattern_core::types::message::{Message, ResponseMeta};
use pattern_core::types::provider::{
    ChatMessage, ChatStreamEvent, CompletionRequest, ToolCall, ToolOutcome, ToolResult,
};
use pattern_core::types::turn::{StepReply, StopReason, TurnCacheMetrics, TurnInput, TurnOutput};

use pattern_provider::compose::passes::{
    Segment1Pass, Segment2Pass, Segment3Pass, synthesize_summary_message,
};
use pattern_provider::compose::{CacheProfile, ComposerPass, PartialRequest, compose};
use pattern_provider::shaper::{ShaperCompatMode, build_system_prompt};

use crate::memory::TurnHistory;
use crate::sdk::CODE_TOOL;
use crate::session::SessionContext;

pub mod eval_worker;

pub use eval_worker::EvalWorker;

// ---- EvalDispatcher -----------------------------------------------------

/// Dispatches a single `code` tool_use to Haskell evaluation, returning
/// the result as a [`ToolOutcome`].
///
/// Abstracted behind a trait so the orchestrator can be exercised in
/// tests with a mock dispatcher (no Haskell compile required) and so
/// the real impl (Task 20 part 5c sibling change — [`EvalWorker`])
/// can be swapped transparently.
///
/// Implementations MUST be `Send + Sync` since the orchestrator calls
/// `dispatch` from the async runtime.
#[async_trait]
pub trait EvalDispatcher: Send + Sync {
    /// Execute one tool call. Arguments are the LLM-emitted JSON (on
    /// `ToolCall::fn_arguments`) and the shared Haskell preamble
    /// (SDK GADT declarations + helpers, assembled once per session).
    ///
    /// Returns a [`ToolOutcome`]:
    /// - `Success(value)` — the Haskell snippet evaluated to a JSON
    ///   value. Sent back to the LLM as the tool_result content.
    /// - `Error(msg)` — compilation failed, runtime error, timeout,
    ///   bad input, etc. Sent back to the LLM so it can recover; does
    ///   NOT propagate as a `Err` from the orchestrator.
    async fn dispatch(&self, tool_call: ToolCall, preamble: &str) -> ToolOutcome;
}

/// No-op dispatcher that always returns an error. Useful for tests
/// that exercise the stream-consumption path without real eval, and
/// as a default placeholder when a session has no configured worker.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpDispatcher;

#[async_trait]
impl EvalDispatcher for NoOpDispatcher {
    async fn dispatch(&self, _tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
        ToolOutcome::Error(
            "no eval dispatcher configured — session opened without a Haskell worker".into(),
        )
    }
}

// ---- orchestrate --------------------------------------------------------

/// Execute one wire turn.
///
/// Returns a populated [`TurnOutput`]:
/// - `messages` — the reconstructed assistant message (with all
///   captured content parts: text, thought signatures if present,
///   tool calls). Caller is responsible for persisting to the
///   session's message log / [`crate::memory::TurnHistory`].
/// - `block_writes` — drained from `ctx.adapter()`'s pending buffer
///   at end of turn.
/// - `tool_calls` / `tool_results` — paired 1:1 by `call_id` when
///   `stop_reason == ToolUse`; both empty otherwise. Per the
///   [`TurnOutput`] invariant.
/// - `stop_reason` — extracted from `StreamEnd.captured_stop_reason`.
/// - `usage` — from `StreamEnd.captured_usage`.
/// - `cache_metrics` — populated from the `Usage.prompt_tokens_details`
///   fields: `cached_tokens` → `cache_read_input_tokens`,
///   `cache_creation_tokens` → `cache_creation_input_tokens`, and the
///   remainder of `prompt_tokens` → `fresh_input_tokens`.
///
/// Errors are returned as `Err(RuntimeError::ProviderError)` for
/// provider-client failures. Tool evaluation failures ride inside
/// `ToolOutcome::Error` on successful returns — they're a normal
/// part of the agent's operation, not orchestrator errors.
///
/// # AC8.5 — segment-1 bust warning
///
/// When `has_segment_1` is `true` (the request placed a segment-1 cache
/// boundary, meaning we expected segment 1 to hit) but the response
/// reports zero `cache_read_input_tokens`, a `tracing::warn!` is emitted
/// to surface the unexpected cache miss for operator visibility.
pub async fn orchestrate(
    req: CompletionRequest,
    input: TurnInput,
    ctx: Arc<SessionContext>,
    dispatcher: &dyn EvalDispatcher,
    preamble: &str,
    has_segment_1: bool,
    expect_segment_1_hit: bool,
) -> Result<TurnOutput, RuntimeError> {
    // 1. Call the provider, consume the stream. Caller is responsible
    //    for having built `req` via the composer pipeline (segments
    //    1/2/3 + fresh input messages appended) — `orchestrate`
    //    itself doesn't know about the cache layout.
    let sink = ctx.turn_sink().clone();

    // Emit the composed request to the sink BEFORE shipping it —
    // consumers (debug UIs, replay snapshot capture, Task 15
    // cache-test observers) tap the request here. NoOpSink drops
    // immediately; subscribers pay a single clone per wire turn.
    sink.emit(TurnEvent::ComposedRequest(Box::new(req.clone())));

    let mut stream =
        ctx.provider()
            .complete(req)
            .await
            .map_err(|e| RuntimeError::ProviderError {
                reason: e.to_string(),
            })?;

    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut captured_reasoning: Option<String> = None;
    let mut captured_content: Option<genai::chat::MessageContent> = None;
    let mut stop_reason = StopReason::EndTurn;
    let mut usage: Option<genai::chat::Usage> = None;

    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| RuntimeError::ProviderError {
            reason: e.to_string(),
        })?;
        match event {
            ChatStreamEvent::Start => {}
            ChatStreamEvent::Chunk(c) => {
                sink.emit(TurnEvent::Text(c.content));
            }
            ChatStreamEvent::ReasoningChunk(c) => {
                sink.emit(TurnEvent::Thinking(c.content));
            }
            ChatStreamEvent::ThoughtSignatureChunk(_) => {
                // Signatures are accumulated into StreamEnd.captured_content
                // at stream end (when the genai fork wires them up for
                // Anthropic). We don't need to do anything with them here.
            }
            ChatStreamEvent::ToolCallChunk(_) => {
                // Incremental tool-call chunks are consolidated into
                // StreamEnd.captured_content at stream end; we read the
                // assembled list from `captured_into_tool_calls` below.
            }
            ChatStreamEvent::End(end) => {
                // Destructure owned fields exactly once each — StreamEnd
                // doesn't impl Copy so we take the fields by move.
                let genai::chat::StreamEnd {
                    captured_usage,
                    captured_stop_reason,
                    captured_content: content_here,
                    captured_reasoning_content,
                    captured_response_id: _,
                } = end;

                if let Some(sr) = captured_stop_reason {
                    stop_reason = map_genai_stop_reason(sr);
                }
                usage = captured_usage;
                captured_reasoning = captured_reasoning_content;

                // Extract tool calls from captured_content before handing
                // the rest of the content to build_assistant_message.
                // We clone the content so we can both surface the tool
                // calls here and reconstruct the assistant message below.
                if let Some(ref content) = content_here {
                    tool_calls = content
                        .parts()
                        .iter()
                        .filter_map(|p| p.as_tool_call().cloned())
                        .collect();
                }
                captured_content = content_here;
            }
        }
    }

    // 3. Dispatch tool calls (if any). Preserve call_id order so
    //    tool_results[i] ↔ tool_calls[i] per the TurnOutput invariant.
    let mut tool_results: Vec<ToolResult> = Vec::new();
    if stop_reason == StopReason::ToolUse && !tool_calls.is_empty() {
        for tc in &tool_calls {
            sink.emit(TurnEvent::ToolCall(tc.clone()));
            let outcome = dispatcher.dispatch(tc.clone(), preamble).await;
            let result = ToolResult {
                call_id: tc.call_id.clone(),
                outcome,
            };
            sink.emit(TurnEvent::ToolResult(result.clone()));
            tool_results.push(result);
        }
    }

    // 4. Build the assistant message from captured content.
    let assistant_message = build_assistant_message(
        captured_content,
        captured_reasoning,
        usage.clone(),
        ctx.agent_id(),
        input.batch_id.clone(),
        ctx.model_id(),
    );

    // 5. Drain pending block writes from the memory adapter.
    let block_writes = ctx.adapter().drain_pending();

    // 6. Build cache metrics from the captured usage.
    //
    // genai's `PromptTokensDetails` uses:
    //   - `cached_tokens`          → cache_read_input_tokens   (0.1× billed)
    //   - `cache_creation_tokens`  → cache_creation_input_tokens (1.25–2× billed)
    //
    // Fresh input tokens are the residual: total prompt tokens minus the
    // two cache buckets. We use saturating subtraction to guard against
    // provider quirks (e.g. zero/None total with non-zero detail fields).
    let cache_metrics = build_cache_metrics(usage.as_ref());

    // 6a. AC8.5 — warn when we placed segment 1 (expected a cache hit on the
    //     stable system-prompt prefix) but the response reported zero cache
    //     reads. This can mean TTL expiry, a content change in segment 1, or
    //     a provider-side regression — all require operator attention.
    //
    //     Gate on `expect_segment_1_hit` too: the VERY first wire turn in a
    //     session has nothing to hit (baseline write), so `cache_read == 0`
    //     there is expected, not a bust. drive_step passes `false` on the
    //     first turn and `true` on subsequent turns in the same exchange;
    //     callers driving orchestrate directly (tests) pass whatever's
    //     semantically correct.
    if has_segment_1 && expect_segment_1_hit && cache_metrics.cache_read_input_tokens == 0 {
        tracing::warn!(
            agent_id = ctx.agent_id(),
            turn_id = %input.turn_id,
            fresh = cache_metrics.fresh_input_tokens,
            cache_create = cache_metrics.cache_creation_input_tokens,
            "segment-1 cache bust: expected cache hit on stable system prefix \
             but cache_read_input_tokens == 0 (TTL expiry, content change, or \
             provider regression)"
        );
    }

    // 6b. Emit per-turn cache metric span for observability.
    tracing::info!(
        agent_id = ctx.agent_id(),
        turn_id = %input.turn_id,
        fresh = cache_metrics.fresh_input_tokens,
        cache_read = cache_metrics.cache_read_input_tokens,
        cache_create = cache_metrics.cache_creation_input_tokens,
        hit_ratio = cache_metrics.hit_ratio(),
        "turn cache metrics"
    );

    // 7. Emit the Stop event and assemble TurnOutput.
    sink.emit(TurnEvent::Stop(stop_reason));

    let messages = match assistant_message {
        Some(m) => vec![m],
        None => vec![],
    };

    Ok(TurnOutput {
        messages,
        block_writes,
        tool_calls,
        tool_results,
        stop_reason,
        usage,
        cache_metrics,
        completed_at: Timestamp::now(),
    })
}

// ---- drive_step — loop driver -------------------------------------------

/// Drive one user-visible exchange: repeatedly call [`orchestrate`]
/// until `stop_reason.is_terminal()`, threading tool_results through
/// [`TurnInput::from_tool_results`] to produce each subsequent wire
/// turn's input.
///
/// Called by [`crate::session::TidepoolSession::step`] as the main
/// user-visible entry point. Preserves `batch_id` across all wire
/// turns; mints a fresh `turn_id` per wire turn (via
/// `TurnInput::from_tool_results`).
///
/// Returns a [`StepReply`] aggregating every wire turn's
/// [`TurnOutput`].
pub async fn drive_step(
    initial_input: TurnInput,
    ctx: Arc<SessionContext>,
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
    cache_profile: CacheProfile,
    dispatcher: &dyn EvalDispatcher,
    preamble: &str,
) -> Result<StepReply, RuntimeError> {
    let batch_id = initial_input.batch_id.clone();
    let agent_id = AgentId::from(ctx.agent_id());
    let mut turns: Vec<TurnOutput> = Vec::new();
    let mut cur_input = initial_input;

    // We expect a cache hit on segment 1 only on turns AFTER the
    // first one in THIS session. Detect via turn_history: if it
    // already has any active messages, prior turns have run and
    // segment 1 SHOULD hit. On a fresh session (empty history +
    // first wire turn), read==0 is the baseline write, not a bust.
    let had_prior_turns = turn_history
        .lock()
        .map(|h| h.active_messages().next().is_some())
        .unwrap_or(false);

    let mut is_first_wire_turn_in_session = !had_prior_turns;

    loop {
        // Build the composed CompletionRequest for THIS wire turn:
        // segments 1 (system + persona + tools) / 2 (prior messages +
        // summary head + pseudo-messages) / 3 (current_state), then
        // fresh input messages appended AFTER compose so they stay
        // uncached (per the three-segment cache layout).
        let (req, has_segment_1) =
            compose_request_for_turn(&ctx, &turn_history, &cur_input, &cache_profile).await?;

        // Expect a segment-1 cache hit on every wire turn AFTER the
        // very first in the session — seg1 is stable, so from turn 2
        // onwards the server should have it cached.
        let expect_segment_1_hit = !is_first_wire_turn_in_session;

        let turn = orchestrate(
            req,
            cur_input,
            ctx.clone(),
            dispatcher,
            preamble,
            has_segment_1,
            expect_segment_1_hit,
        )
        .await?;

        is_first_wire_turn_in_session = false;
        let terminal = turn.stop_reason.is_terminal();
        let needs_next =
            matches!(turn.stop_reason, StopReason::ToolUse) && !turn.tool_results.is_empty();

        // Record into TurnHistory so the NEXT wire turn's composer
        // sees this turn's messages + block_writes in segment 2.
        if let Ok(mut hist) = turn_history.lock() {
            hist.record(pattern_core::types::ids::new_id(), turn.clone());
        }

        turns.push(turn);

        if terminal || !needs_next {
            break;
        }

        // Build the next wire turn's input from this turn's tool_results.
        // Safe to unwrap: we just pushed above.
        let prior = turns.last().expect("just pushed");
        cur_input = TurnInput::from_tool_results(prior, batch_id.clone(), agent_id.clone());
    }

    let final_stop_reason = turns
        .last()
        .map(|t| t.stop_reason)
        .unwrap_or(StopReason::EndTurn);
    let total_usage = aggregate_usage(&turns);

    Ok(StepReply {
        turns,
        final_stop_reason,
        total_usage,
    })
}

// ---- composer integration ----------------------------------------------

/// Build the composed [`CompletionRequest`] for one wire turn.
///
/// Runs the three-segment composer pipeline:
///
/// - **Segment 1** — system prompt (persona + [`pattern_core::DEFAULT_BASE_INSTRUCTIONS`]
///   via [`build_system_prompt`]) + [`CODE_TOOL`] in tools. Cache
///   boundary marker placed per `cache_profile.segment_1_control()`.
/// - **Segment 2** — summary-head synthesized from
///   [`TurnHistory::summary_head`] + prior messages from
///   [`TurnHistory::active_messages`] +
///   [`TurnHistory::most_recent_block_writes`] rendered as
///   pseudo-messages. Cache marker per
///   `cache_profile.segment_2_control()`.
/// - **Segment 3** — `[memory:current_state]` pseudo-turn. Phase 5
///   ships with empty blocks (loaded-blocks concept is future scope);
///   the pass still emits the tag + boundary marker so cache
///   placement stays consistent.
///
/// Fresh `input.messages` are appended AFTER `compose` returns, so
/// they sit past the segment-3 cache boundary (stay uncached — fresh
/// user input bursts cache downstream content by design).
///
/// Returns `(request, has_segment_1)` where `has_segment_1` is `true`
/// when the composer placed at least one system block in segment 1.
/// The caller passes this flag to [`orchestrate`] for the AC8.5
/// segment-1 bust warning.
///
/// Today's limitations:
///
/// - `ShaperCompatMode` is hardcoded to `SubscriptionRoutingShape`.
///   Session-level override is future work (Phase 5 follow-up).
/// - Segment 3's `blocks` vec is always empty. When the runtime
///   grows a "which blocks are loaded in context" registry, wire it
///   here.
async fn compose_request_for_turn(
    ctx: &Arc<SessionContext>,
    turn_history: &std::sync::Mutex<TurnHistory>,
    input: &TurnInput,
    cache_profile: &CacheProfile,
) -> Result<(CompletionRequest, bool), RuntimeError> {
    // 1. Load persona from memory (best-effort — no persona block is
    //    a valid state; the system prompt gracefully degrades to just
    //    base instructions).
    let persona_text = ctx
        .memory_store()
        .get_block(ctx.agent_id(), pattern_core::PERSONA_LABEL)
        .await
        .ok()
        .flatten()
        .map(|doc| doc.render())
        .unwrap_or_default();

    // 2. Build system_blocks via the shaper. ShaperCompatMode is
    //    hardcoded to SubscriptionRoutingShape today — see function
    //    doc for the rationale.
    let mode = default_shaper_mode();
    let system_blocks = build_system_prompt(
        mode,
        pattern_core::DEFAULT_BASE_INSTRUCTIONS,
        &persona_text,
        &[],
    );

    // 3. Snapshot TurnHistory state. Holding the mutex across the
    //    persona-load await above would be a deadlock risk — we
    //    acquire briefly here only.
    let (summary_head_messages, prior_messages, recent_block_writes) = {
        let hist = turn_history
            .lock()
            .map_err(|_| RuntimeError::ProviderError {
                reason: "turn_history mutex poisoned".into(),
            })?;

        let summary_head_messages: Vec<ChatMessage> = hist
            .summary_head()
            .iter()
            .map(|s| {
                synthesize_summary_message(s.depth, &s.start_position, &s.end_position, &s.summary)
            })
            .collect();

        let prior_messages: Vec<ChatMessage> = hist
            .active_messages()
            .map(|m| m.chat_message.clone())
            .collect();

        let recent_block_writes = hist.most_recent_block_writes().to_vec();

        (summary_head_messages, prior_messages, recent_block_writes)
    };

    // 4. Load segment-3 blocks: all agent blocks EXCEPT the persona
    //    (which already lives in segment 1's system prompt — loading
    //    it twice would double-count cache + token cost).
    //
    //    Today this means "every block attached to this agent" —
    //    there's no per-conversation selection of which blocks are
    //    in-context. A more selective loader (only blocks referenced
    //    in the current turn, or explicitly-loaded blocks tracked
    //    per session) is future refinement; the current shape at
    //    least makes segment 3 carry real content so the cache
    //    behaviour matches the plan's design.
    let mut loaded_blocks: Vec<StructuredDocument> = Vec::new();
    let block_list = ctx
        .memory_store()
        .list_blocks(ctx.agent_id())
        .await
        .map_err(|e| RuntimeError::ProviderError {
            reason: format!("list_blocks failed: {e}"),
        })?;
    for meta in block_list {
        if meta.label == pattern_core::PERSONA_LABEL {
            continue;
        }
        if let Some(doc) = ctx
            .memory_store()
            .get_block(ctx.agent_id(), &meta.label)
            .await
            .map_err(|e| RuntimeError::ProviderError {
                reason: format!("get_block({}) failed: {e}", meta.label),
            })?
        {
            loaded_blocks.push(doc);
        }
    }

    // 5. Record whether segment 1 has content before `system_blocks`
    //    is moved into the pass. `build_system_prompt` always emits at
    //    least base-instructions, so this is almost always `true` — we
    //    track it explicitly so the AC8.5 bust warning has a reliable
    //    predicate rather than guessing.
    let has_segment_1 = !system_blocks.is_empty();

    // 6. Detect tool-continuation turns. Anthropic's wire protocol
    //    requires that an assistant message containing `tool_use`
    //    blocks be IMMEDIATELY followed by a user message with
    //    matching `tool_result` blocks — no pseudo-messages,
    //    current-state stubs, or other user-role content may
    //    intervene. When the input is `tool_results` (built via
    //    `TurnInput::from_tool_results`), the naive segment-3 pass
    //    emits its pseudo-user message between the prior
    //    assistant(tool_use) and our user(tool_result), which
    //    Anthropic 400s with "tool_use ids were found without
    //    tool_result blocks immediately after".
    //
    //    Our fix matches claude-code's convention (see
    //    smooshSystemReminderSiblings in their utils/messages.ts):
    //    splice the segment-3 text IN FRONT OF the tool_result
    //    content parts inside the SAME user message. Anthropic
    //    accepts multiple content parts per user message as long as
    //    the tool_result block is present; the composer's
    //    segment-3 cache_control marker lands on the last content
    //    block (via genai's apply_cache_control_to_parts), which
    //    IS the tool_result — the cache span still includes the
    //    prepended segment-3 text earlier in the same message.
    let is_tool_continuation = input
        .messages
        .iter()
        .any(|m| m.chat_message.role == genai::chat::ChatRole::Tool);

    // Assemble the pass list. Segment3Pass runs on non-continuation
    // turns; on continuation turns we splice manually after
    // compose() below (Segment3Pass would emit a free-standing
    // pseudo-user message that violates Anthropic's adjacency rule).
    let mut passes: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(
            system_blocks,
            vec![CODE_TOOL.clone()],
            cache_profile.clone(),
        )),
        Box::new(Segment2Pass::new(
            summary_head_messages,
            prior_messages,
            &recent_block_writes,
            cache_profile.clone(),
        )),
    ];
    let mut segment_3_for_splice: Option<Vec<StructuredDocument>> = None;
    if is_tool_continuation {
        segment_3_for_splice = Some(loaded_blocks);
    } else {
        passes.push(Box::new(Segment3Pass::new(
            loaded_blocks,
            cache_profile.clone(),
        )));
    }

    let initial = PartialRequest::new(ctx.model_id());
    let mut req = compose(&passes, initial).map_err(|e| RuntimeError::ProviderError {
        reason: format!("composer pipeline failed: {e}"),
    })?;

    // 7. Enable capture flags on ChatOptions so the genai streamer
    //    populates StreamEnd with usage / content / tool_calls /
    //    reasoning. Without these, the Anthropic streamer silently
    //    drops the fields and the agent loop sees empty
    //    TurnOutput.usage / cache_metrics and no captured tool_calls,
    //    which breaks drive_step's loop termination logic.
    req.options = req
        .options
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_capture_reasoning_content(true);

    // 8. Append fresh input messages AFTER compose so they sit
    //    beyond the segment-3 cache boundary (uncached by design).
    for msg in &input.messages {
        req.chat.messages.push(msg.chat_message.clone());
    }

    // 9. On tool-continuation turns, splice segment 3 INTO the last
    //    ToolResponse's content array. We fold the seg3 text as a
    //    nested block inside tool_result.content rather than emitting
    //    it as a preceding sibling content part.
    //
    //    Anthropic's docs ("Important formatting requirements") state:
    //    "In the user message containing tool results, the tool_result
    //    blocks must come FIRST in the content array. Any text must
    //    come AFTER all tool results." Prepending a Text sibling before
    //    tool_result causes a 400. Folding into tool_result.content
    //    matches Anthropic's documented format (tool_result.content
    //    may be a string OR an array of text/image/document blocks)
    //    and mirrors claude-code's production `smooshIntoToolResult`
    //    pattern. Role stays ChatRole::Tool — no flip needed.
    if let Some(blocks) = segment_3_for_splice {
        use genai::chat::{ChatRole, ContentPart, MessageContent};

        let seg3_msg = pattern_provider::compose::current_state::render_current_state(&blocks);
        let seg3_text = seg3_msg
            .content
            .joined_texts()
            .unwrap_or_else(|| "[memory:current_state]\n(no blocks loaded)".into());

        if let Some(last_tool_msg) = req
            .chat
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == ChatRole::Tool)
        {
            // Walk the parts in reverse to find the LAST ToolResponse
            // and fold seg3 into its content. We rebuild the parts vec
            // so we can replace the matched part in place.
            let original_parts = last_tool_msg.content.parts().clone();
            let mut new_parts: Vec<ContentPart> = Vec::with_capacity(original_parts.len());
            let mut folded = false;

            // Iterate in reverse, fold once on the first (last) ToolResponse.
            for part in original_parts.into_iter().rev() {
                if !folded && let ContentPart::ToolResponse(mut tr) = part {
                    // Build the folded content array:
                    //   - First element: seg3 text block (so it appears
                    //     "first" within tool_result.content when read
                    //     top-to-bottom — Anthropic renders inner blocks
                    //     in order, and prepending gives the model context
                    //     before the tool result).
                    //   - Remaining elements: original content preserved
                    //     verbatim per its existing shape.
                    let seg3_block = serde_json::json!({"type": "text", "text": seg3_text});

                    let folded_content = match tr.content {
                        // Plain string → wrap as a text block after seg3.
                        serde_json::Value::String(ref s) => {
                            serde_json::json!([
                                seg3_block,
                                {"type": "text", "text": s},
                            ])
                        }
                        // Existing array → prepend seg3 block.
                        serde_json::Value::Array(ref items) => {
                            let mut arr = Vec::with_capacity(items.len() + 1);
                            arr.push(seg3_block);
                            arr.extend(items.iter().cloned());
                            serde_json::Value::Array(arr)
                        }
                        // Null, Object, Bool, Number → stringify and
                        // wrap as text; shouldn't occur in practice but
                        // handled explicitly to avoid silent loss.
                        ref other => {
                            serde_json::json!([
                                seg3_block,
                                {"type": "text", "text": other.to_string()},
                            ])
                        }
                    };
                    tr.content = folded_content;
                    new_parts.push(ContentPart::ToolResponse(tr));
                    folded = true;
                } else {
                    new_parts.push(part);
                }
            }
            // Restore forward order (we iterated in reverse).
            new_parts.reverse();
            last_tool_msg.content = MessageContent::from_parts(new_parts);

            // Role stays ChatRole::Tool. The Anthropic adapter serializes
            // Tool-role messages correctly as user-role "tool_result"
            // blocks on the wire. There is no need to flip to User.

            // Apply segment-3 cache_control so the spliced seg3 +
            // tool_result message is the cache boundary. Note: this
            // marker is applied directly to the ChatMessage options
            // rather than via the composer's BreakpointTracker — it
            // bypasses the 4-marker budget check, but seg1+seg2+seg3
            // = 3 markers so we're still under the Anthropic limit.
            // Break-detection hashing won't capture this marker;
            // observability gap noted for follow-up.
            let opts = last_tool_msg
                .options
                .clone()
                .unwrap_or_default()
                .with_cache_control(cache_profile.segment_3_control());
            last_tool_msg.options = Some(opts);
        }
    }

    Ok((req, has_segment_1))
}

/// Default `ShaperCompatMode` used by the composer. Hardcoded to
/// `SubscriptionRoutingShape` when built with the
/// `subscription-oauth` feature, `HonestPattern` otherwise. A future
/// refinement may expose this as a session-level override.
#[cfg(feature = "subscription-oauth")]
fn default_shaper_mode() -> ShaperCompatMode {
    ShaperCompatMode::SubscriptionRoutingShape
}

#[cfg(not(feature = "subscription-oauth"))]
fn default_shaper_mode() -> ShaperCompatMode {
    ShaperCompatMode::HonestPattern
}

// ---- helpers ------------------------------------------------------------

/// Build [`TurnCacheMetrics`] from an optional genai [`Usage`].
///
/// Extracts cache token counts from `usage.prompt_tokens_details`:
/// - `cached_tokens` → `cache_read_input_tokens`
/// - `cache_creation_tokens` → `cache_creation_input_tokens`
///
/// Fresh input tokens are the residual: `prompt_tokens` minus the two
/// cache buckets. Saturating subtraction guards against provider quirks
/// where detail buckets might exceed the reported total.
///
/// Returns `TurnCacheMetrics::default()` (all zeroes) when `usage` is
/// `None` (provider did not report usage for this turn).
fn build_cache_metrics(usage: Option<&genai::chat::Usage>) -> TurnCacheMetrics {
    let Some(usage) = usage else {
        return TurnCacheMetrics::default();
    };

    let details = usage.prompt_tokens_details.as_ref();

    let cache_read = details
        .and_then(|d| d.cached_tokens)
        .map(|v| v.max(0) as u64)
        .unwrap_or(0);

    let cache_create = details
        .and_then(|d| d.cache_creation_tokens)
        .map(|v| v.max(0) as u64)
        .unwrap_or(0);

    let total_prompt = usage.prompt_tokens.map(|v| v.max(0) as u64).unwrap_or(0);

    // Fresh tokens = total input − cache_read − cache_create.
    // We use saturating_sub in case the provider's accounting has
    // rounding quirks.
    let fresh = total_prompt
        .saturating_sub(cache_read)
        .saturating_sub(cache_create);

    TurnCacheMetrics::new(fresh, cache_read, cache_create)
}

/// Map genai's provider-agnostic `StopReason` → pattern-core's
/// wire-level `StopReason`.
fn map_genai_stop_reason(reason: genai::chat::StopReason) -> StopReason {
    match reason {
        genai::chat::StopReason::Completed(_) => StopReason::EndTurn,
        genai::chat::StopReason::MaxTokens(_) => StopReason::MaxTokens,
        genai::chat::StopReason::ToolCall(_) => StopReason::ToolUse,
        genai::chat::StopReason::ContentFilter(_) => StopReason::Refusal,
        genai::chat::StopReason::StopSequence(_) => StopReason::StopSequence,
        genai::chat::StopReason::Other(raw) => {
            // Anthropic's server-tool pause shows up as a pass-through
            // string; other providers may surface similar things. Map
            // the known cases; anything genuinely unknown falls back
            // to EndTurn (terminal, won't loop forever).
            match raw.as_str() {
                "pause_turn" | "PAUSE_TURN" => StopReason::PauseTurn,
                _ => StopReason::EndTurn,
            }
        }
    }
}

/// Build the assistant message from captured stream content. Returns
/// `None` if the stream produced no content at all (shouldn't happen
/// in practice — even an empty response has a Stop event). The
/// `reasoning_content` (if present) attaches via [`ResponseMeta`] so
/// it's preserved on the persisted message.
fn build_assistant_message(
    content: Option<genai::chat::MessageContent>,
    reasoning: Option<String>,
    usage: Option<genai::chat::Usage>,
    agent_id: &str,
    batch_id: pattern_core::types::ids::BatchId,
    model_id: &str,
) -> Option<Message> {
    let content = content?;
    let chat_message = ChatMessage::assistant(content);

    // Derive ModelIden from the session's configured model string.
    // genai's StreamEnd doesn't surface model identities today, so we
    // reconstruct from what we know: `ctx.model_id()` via
    // `AdapterKind::from_model`. Both `model_iden` and
    // `provider_model_iden` point to the same identity since we
    // don't have a distinct "provider's view" of the model ID at
    // this layer — providers that alias (e.g. Bedrock mapping a
    // canonical name to a provider-specific one) can override
    // `provider_model_iden` when their adapter surfaces the
    // alias; Phase 5 uses the one identity.
    let model_iden = session_model_iden(model_id);
    let response_meta = usage.map(|u| ResponseMeta {
        usage: u,
        reasoning_content: reasoning,
        model_iden: model_iden.clone(),
        provider_model_iden: model_iden,
    });

    Some(Message {
        chat_message,
        id: MessageId::from(new_id()),
        owner_id: AgentId::from(agent_id),
        created_at: Timestamp::now(),
        batch: batch_id,
        response_meta,
        block_refs: vec![],
    })
}

/// Build a [`genai::ModelIden`] from the session's configured model
/// string, deriving the adapter kind via
/// [`genai::adapter::AdapterKind::from_model`]. Unknown model
/// prefixes fall back to `Anthropic` — Pattern's current foundation
/// target — so downstream code doesn't blow up on novel model names.
fn session_model_iden(model_id: &str) -> genai::ModelIden {
    let adapter_kind = genai::adapter::AdapterKind::from_model(model_id)
        .unwrap_or(genai::adapter::AdapterKind::Anthropic);
    genai::ModelIden::new(adapter_kind, genai::ModelName::new(model_id.to_string()))
}

/// Sum usage across every wire turn. Returns `None` when no turn
/// reported usage.
fn aggregate_usage(turns: &[TurnOutput]) -> Option<genai::chat::Usage> {
    turns
        .iter()
        .filter_map(|t| t.usage.as_ref())
        .cloned()
        .reduce(merge_usage)
}

/// Merge two genai `Usage` snapshots by summing the token counts.
/// genai's `Usage` doesn't impl `Add`, so we roll our own.
fn merge_usage(a: genai::chat::Usage, b: genai::chat::Usage) -> genai::chat::Usage {
    use genai::chat::Usage;
    Usage {
        prompt_tokens: sum_opt(a.prompt_tokens, b.prompt_tokens),
        completion_tokens: sum_opt(a.completion_tokens, b.completion_tokens),
        total_tokens: sum_opt(a.total_tokens, b.total_tokens),
        prompt_tokens_details: a.prompt_tokens_details.or(b.prompt_tokens_details),
        completion_tokens_details: a.completion_tokens_details.or(b.completion_tokens_details),
    }
}

fn sum_opt(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

// ---- tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    fn map_stop_reason_covers_known_variants() {
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::Completed("end_turn".into())),
            StopReason::EndTurn
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::MaxTokens("max_tokens".into())),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::ToolCall("tool_use".into())),
            StopReason::ToolUse
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::ContentFilter("SAFETY".into())),
            StopReason::Refusal
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::StopSequence(
                "stop_sequence".into()
            )),
            StopReason::StopSequence
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::Other("pause_turn".into())),
            StopReason::PauseTurn
        );
        assert_eq!(
            map_genai_stop_reason(genai::chat::StopReason::Other("weird_unknown".into())),
            StopReason::EndTurn
        );
    }

    #[tokio::test]
    async fn noop_dispatcher_returns_error_outcome() {
        let dispatcher = NoOpDispatcher;
        let tc = ToolCall {
            call_id: "toolu_1".into(),
            fn_name: "code".into(),
            fn_arguments: serde_json::json!({"code": "pure ()"}),
            thought_signatures: None,
        };
        let outcome = dispatcher.dispatch(tc, "").await;
        match outcome {
            ToolOutcome::Error(msg) => assert!(msg.contains("no eval dispatcher")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn merge_usage_sums_known_fields() {
        use genai::chat::Usage;
        let a = Usage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };
        let b = Usage {
            prompt_tokens: Some(200),
            completion_tokens: Some(80),
            total_tokens: Some(280),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };
        let merged = merge_usage(a, b);
        assert_eq!(merged.prompt_tokens, Some(300));
        assert_eq!(merged.completion_tokens, Some(130));
        assert_eq!(merged.total_tokens, Some(430));
    }

    #[test]
    fn merge_usage_handles_missing_halves() {
        use genai::chat::Usage;
        let a = Usage {
            prompt_tokens: Some(100),
            completion_tokens: None,
            total_tokens: None,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };
        let b = Usage {
            prompt_tokens: None,
            completion_tokens: Some(50),
            total_tokens: None,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };
        let merged = merge_usage(a, b);
        assert_eq!(merged.prompt_tokens, Some(100));
        assert_eq!(merged.completion_tokens, Some(50));
        assert_eq!(merged.total_tokens, None);
    }

    #[test]
    fn aggregate_usage_empty_turns_returns_none() {
        let turns: Vec<TurnOutput> = vec![];
        assert!(aggregate_usage(&turns).is_none());
    }

    // ---- Integration tests: orchestrate + drive_step via MockProviderClient ----

    use crate::testing::{InMemoryMemoryStore, MockProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
    use pattern_core::types::ids::{BatchId, new_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaConfig;

    /// Build a SessionContext wired to a MockProviderClient returning
    /// the given scripted turns. Returns `(ctx, vec_sink, provider)`.
    /// The provider is returned separately so tests can assert
    /// `call_count` post-run.
    fn mock_session(
        turns: Vec<Vec<genai::chat::ChatStreamEvent>>,
    ) -> (Arc<SessionContext>, Arc<VecSink>, Arc<MockProviderClient>) {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider_concrete = Arc::new(MockProviderClient::with_turns(turns));
        let provider: Arc<dyn ProviderClient> = provider_concrete.clone();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();
        let persona = PersonaConfig::new("agent-a", "A", "module X where\nx = pure ()");
        let ctx = Arc::new(
            SessionContext::from_persona(&persona, store, provider).with_turn_sink(sink_dyn),
        );
        (ctx, sink, provider_concrete)
    }

    fn test_turn_input() -> TurnInput {
        TurnInput {
            turn_id: new_id(),
            batch_id: BatchId::from(new_id()),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![],
        }
    }

    /// Minimal `CompletionRequest` for orchestrate unit tests — they
    /// exercise stream consumption + tool dispatch, not the composer.
    fn simple_req() -> CompletionRequest {
        CompletionRequest::new("claude-sonnet-4-20250514")
    }

    /// NoOpDispatcher returns Error outcomes; useful for tests that
    /// don't exercise the tool path.
    #[tokio::test]
    async fn orchestrate_text_only_turn_produces_end_turn_output() {
        let (ctx, sink, provider) =
            mock_session(vec![MockProviderClient::text_turn("Hello, world!")]);

        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        assert_eq!(provider.call_count(), 1);
        assert_eq!(out.stop_reason, StopReason::EndTurn);
        assert!(out.tool_calls.is_empty());
        assert!(out.tool_results.is_empty());
        assert_eq!(out.messages.len(), 1, "assistant message should be present");
        assert!(out.usage.is_some(), "usage captured from StreamEnd");

        // Sink events: Start(n/a), Text, Stop
        let events = sink.snapshot();
        let text_count = events
            .iter()
            .filter(|e| matches!(e, TurnEvent::Text(_)))
            .count();
        assert_eq!(text_count, 1);
        assert!(
            matches!(events.last(), Some(TurnEvent::Stop(StopReason::EndTurn))),
            "last event should be Stop(EndTurn), got {:?}",
            events.last()
        );
    }

    #[tokio::test]
    async fn orchestrate_thinking_then_text_surfaces_thinking_events() {
        let (ctx, sink, _) = mock_session(vec![MockProviderClient::thinking_then_text_turn(
            "The user asks about weather...",
            "It's sunny today.",
        )]);

        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        assert_eq!(out.stop_reason, StopReason::EndTurn);
        let msg = out.messages.first().expect("assistant message");
        let reasoning = msg
            .response_meta
            .as_ref()
            .and_then(|r| r.reasoning_content.as_deref());
        assert_eq!(reasoning, Some("The user asks about weather..."));

        let events = sink.snapshot();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::Thinking(s) if s.contains("user asks"))),
            "sink should contain Thinking event, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::Text(s) if s.contains("sunny"))),
            "sink should contain Text event, got: {events:?}"
        );
    }

    /// Mock dispatcher that always succeeds with a fixed JSON payload,
    /// recording every call.
    #[derive(Debug, Default)]
    struct MockSuccessDispatcher {
        calls: std::sync::Mutex<Vec<ToolCall>>,
    }

    #[async_trait]
    impl EvalDispatcher for MockSuccessDispatcher {
        async fn dispatch(&self, tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
            self.calls.lock().unwrap().push(tool_call);
            ToolOutcome::Success(serde_json::json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn orchestrate_tool_use_dispatches_eval_and_returns_populated_results() {
        let (ctx, sink, _) = mock_session(vec![MockProviderClient::tool_use_turn(
            "toolu_01",
            "code",
            serde_json::json!({"code": "put \"notes\" \"hi\""}),
        )]);

        let dispatcher = MockSuccessDispatcher::default();
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        assert_eq!(out.stop_reason, StopReason::ToolUse);
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].call_id, "toolu_01");
        assert_eq!(out.tool_results.len(), 1);
        assert_eq!(out.tool_results[0].call_id, "toolu_01");
        assert!(
            matches!(out.tool_results[0].outcome, ToolOutcome::Success(_)),
            "dispatcher should have succeeded"
        );

        let calls = dispatcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].fn_name, "code");

        // Sink: ToolCall + ToolResult + Stop events.
        let events = sink.snapshot();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::ToolCall(tc) if tc.call_id == "toolu_01"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::ToolResult(tr) if tr.call_id == "toolu_01"))
        );
        assert!(matches!(
            events.last(),
            Some(TurnEvent::Stop(StopReason::ToolUse))
        ));
    }

    #[tokio::test]
    async fn drive_step_chains_tool_use_then_final_text_into_two_wire_turns() {
        let (ctx, sink, provider) = mock_session(vec![
            // Wire turn 1: emit tool_use
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "pure ()"}),
            ),
            // Wire turn 2: final answer
            MockProviderClient::text_turn("I ran your code."),
        ]);

        let dispatcher = MockSuccessDispatcher::default();
        let reply = drive_step(
            test_turn_input(),
            ctx,
            Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty())),
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed");

        assert_eq!(provider.call_count(), 2, "two wire turns expected");
        assert_eq!(reply.turns.len(), 2);
        assert_eq!(reply.turns[0].stop_reason, StopReason::ToolUse);
        assert_eq!(reply.turns[1].stop_reason, StopReason::EndTurn);
        assert_eq!(reply.final_stop_reason, StopReason::EndTurn);

        // Batch id stable across wire turns.
        assert_eq!(
            reply.turns[0].messages[0].batch, reply.turns[1].messages[0].batch,
            "all wire turns in one step share batch_id"
        );

        // Aggregate usage summed from both turns.
        let agg = reply.total_usage.expect("aggregated usage present");
        // Turn 1 tool_use_turn: prompt=50, Turn 2 text_turn: prompt=10 → 60
        assert_eq!(agg.prompt_tokens, Some(60));

        // The sink sees two Stop events (one per wire turn).
        let stop_count = sink
            .snapshot()
            .iter()
            .filter(|e| matches!(e, TurnEvent::Stop(_)))
            .count();
        assert_eq!(stop_count, 2);
    }

    // ---- Cache metrics tests ------------------------------------------------

    #[tokio::test]
    async fn orchestrate_populates_cache_metrics_from_usage() {
        use genai::chat::{PromptTokensDetails, Usage};

        // Build a Usage with known cache fields:
        //   prompt_tokens = 1000 (total)
        //   cached_tokens = 800  (cache reads)
        //   cache_creation_tokens = 50 (new entries)
        //   fresh = 1000 - 800 - 50 = 150
        let cache_usage = Usage {
            prompt_tokens: Some(1000),
            completion_tokens: Some(50),
            total_tokens: Some(1050),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(800),
                cache_creation_tokens: Some(50),
                cache_creation_details: None,
                audio_tokens: None,
            }),
            completion_tokens_details: None,
        };

        let (ctx, _sink, _provider) = mock_session(vec![MockProviderClient::text_turn_with_usage(
            "cached response",
            cache_usage,
        )]);

        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        let m = &out.cache_metrics;
        assert_eq!(m.cache_read_input_tokens, 800, "cache_read should be 800");
        assert_eq!(
            m.cache_creation_input_tokens, 50,
            "cache_creation should be 50"
        );
        assert_eq!(m.fresh_input_tokens, 150, "fresh should be 1000-800-50=150");
        assert_eq!(m.total_input_tokens(), 1000);
        // hit ratio: 800 / (800+150) ≈ 0.842
        assert!(
            (m.hit_ratio() - 800.0 / 950.0).abs() < 1e-9,
            "hit_ratio mismatch: {}",
            m.hit_ratio()
        );
    }

    #[tokio::test]
    async fn orchestrate_cache_metrics_default_when_usage_absent() {
        use genai::chat::{ChatStreamEvent, StreamEnd};

        // Construct a turn that reports no usage at all.
        let no_usage_turn = vec![
            ChatStreamEvent::Start,
            ChatStreamEvent::Chunk(genai::chat::StreamChunk {
                content: "hello".into(),
            }),
            ChatStreamEvent::End(StreamEnd {
                captured_usage: None,
                captured_stop_reason: Some(genai::chat::StopReason::Completed("end_turn".into())),
                captured_content: Some(genai::chat::MessageContent::from_text("hello")),
                captured_reasoning_content: None,
                captured_response_id: None,
            }),
        ];

        let (ctx, _sink, _) = mock_session(vec![no_usage_turn]);
        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        let m = &out.cache_metrics;
        assert_eq!(m.cache_read_input_tokens, 0);
        assert_eq!(m.cache_creation_input_tokens, 0);
        assert_eq!(m.fresh_input_tokens, 0);
        assert_eq!(m.hit_ratio(), 0.0);
    }

    #[tokio::test]
    async fn orchestrate_cache_metrics_all_fresh_when_no_details() {
        // Usage with prompt_tokens but no prompt_tokens_details.
        // All tokens should be counted as fresh.
        use genai::chat::Usage;
        let fresh_usage = Usage {
            prompt_tokens: Some(500),
            completion_tokens: Some(20),
            total_tokens: Some(520),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };

        let (ctx, _sink, _) = mock_session(vec![MockProviderClient::text_turn_with_usage(
            "fresh response",
            fresh_usage,
        )]);
        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            false,
            false,
        )
        .await
        .expect("orchestrate should succeed");

        let m = &out.cache_metrics;
        assert_eq!(m.fresh_input_tokens, 500);
        assert_eq!(m.cache_read_input_tokens, 0);
        assert_eq!(m.cache_creation_input_tokens, 0);
        assert_eq!(m.hit_ratio(), 0.0);
    }

    // ---- AC8.5: segment-1 bust warning tests --------------------------------

    /// When `has_segment_1 = true` and the response reports zero
    /// `cache_read_input_tokens`, `orchestrate` must emit a `tracing::warn`
    /// that includes "segment-1 cache bust".
    #[traced_test]
    #[tokio::test]
    async fn orchestrate_emits_segment1_bust_warning_when_cache_read_zero_with_segment1() {
        use genai::chat::Usage;

        // All-fresh usage: no cache reads, has_segment_1 = true.
        let fresh_usage = Usage {
            prompt_tokens: Some(1000),
            completion_tokens: Some(50),
            total_tokens: Some(1050),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };

        let (ctx, _sink, _) = mock_session(vec![MockProviderClient::text_turn_with_usage(
            "segment 1 busted",
            fresh_usage,
        )]);

        let dispatcher = NoOpDispatcher;
        let out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            true, // has_segment_1 = true
            true, // expect_segment_1_hit = true → bust warning expected
        )
        .await
        .expect("orchestrate should succeed");

        assert_eq!(out.cache_metrics.cache_read_input_tokens, 0);
        assert!(
            logs_contain("segment-1 cache bust"),
            "expected segment-1 bust warning in tracing output"
        );
    }

    /// When `has_segment_1 = true` but the response reports nonzero
    /// `cache_read_input_tokens`, no bust warning should be emitted.
    #[traced_test]
    #[tokio::test]
    async fn orchestrate_no_bust_warning_when_cache_read_nonzero() {
        use genai::chat::{PromptTokensDetails, Usage};

        let cache_usage = Usage {
            prompt_tokens: Some(1000),
            completion_tokens: Some(50),
            total_tokens: Some(1050),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(900),
                cache_creation_tokens: None,
                cache_creation_details: None,
                audio_tokens: None,
            }),
            completion_tokens_details: None,
        };

        let (ctx, _sink, _) = mock_session(vec![MockProviderClient::text_turn_with_usage(
            "cache hit response",
            cache_usage,
        )]);

        let dispatcher = NoOpDispatcher;
        let _out = orchestrate(
            simple_req(),
            test_turn_input(),
            ctx,
            &dispatcher,
            "",
            true, // has_segment_1 = true
            true, // expect_segment_1_hit = true, but cache_read > 0 → no warn
        )
        .await
        .expect("orchestrate should succeed");

        assert!(
            !logs_contain("segment-1 cache bust"),
            "should NOT emit bust warning when cache_read > 0"
        );
    }

    /// Dispatcher that always returns Error; exercises the error-path.
    #[derive(Debug, Default)]
    struct ErrorDispatcher;

    #[async_trait]
    impl EvalDispatcher for ErrorDispatcher {
        async fn dispatch(&self, _tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
            ToolOutcome::Error("eval failed: syntax error at line 1".into())
        }
    }

    #[tokio::test]
    async fn drive_step_tool_error_feeds_back_then_final_text() {
        let (ctx, _sink, provider) = mock_session(vec![
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "broken haskell"}),
            ),
            MockProviderClient::text_turn("Sorry, my code was broken."),
        ]);

        let dispatcher = ErrorDispatcher;
        let reply = drive_step(
            test_turn_input(),
            ctx,
            Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty())),
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed even when tool errors");

        assert_eq!(provider.call_count(), 2);
        assert_eq!(reply.turns.len(), 2);
        let outcome = &reply.turns[0].tool_results[0].outcome;
        assert!(
            matches!(outcome, ToolOutcome::Error(msg) if msg.contains("syntax error")),
            "tool result should carry the error outcome"
        );
        assert_eq!(reply.final_stop_reason, StopReason::EndTurn);
    }

    // ---- Seg3 splice unit tests --------------------------------------------
    //
    // These tests exercise the splice logic in isolation: construct a Tool-role
    // message, apply the same fold that `compose_request_for_turn` does, and
    // assert the resulting shape is correct.
    //
    // They are regression guards for the Anthropic wire-format requirement:
    // tool_result blocks must NOT have a preceding text sibling in the same
    // user message — instead the seg3 text is folded INTO the ToolResponse
    // content array, matching Anthropic's documented nested-block format and
    // claude-code's `smooshIntoToolResult` pattern.

    /// Helper: apply the same fold as the production splice to a single
    /// ToolResponse part with the given original content, returning the
    /// rewritten content Value.
    fn apply_seg3_fold(original_content: serde_json::Value, seg3_text: &str) -> serde_json::Value {
        let seg3_block = serde_json::json!({"type": "text", "text": seg3_text});

        match original_content {
            serde_json::Value::String(ref s) => {
                serde_json::json!([
                    seg3_block,
                    {"type": "text", "text": s},
                ])
            }
            serde_json::Value::Array(ref items) => {
                let mut arr = Vec::with_capacity(items.len() + 1);
                arr.push(seg3_block);
                arr.extend(items.iter().cloned());
                serde_json::Value::Array(arr)
            }
            ref other => {
                serde_json::json!([
                    seg3_block,
                    {"type": "text", "text": other.to_string()},
                ])
            }
        }
    }

    /// When the original ToolResponse content is a plain string, the fold
    /// should produce a two-element array: [seg3 text block, original text block].
    #[test]
    fn seg3_splice_string_content_produces_two_block_array() {
        let original = serde_json::Value::String("tool output here".into());
        let folded = apply_seg3_fold(original, "seg3 memory context");

        let arr = folded.as_array().expect("folded content must be an array");
        assert_eq!(arr.len(), 2, "must have exactly two blocks");

        // First block: seg3 text.
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "seg3 memory context");

        // Second block: original tool output.
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], "tool output here");
    }

    /// When the original content is already an array of blocks, the fold
    /// should prepend the seg3 block, preserving all existing elements.
    #[test]
    fn seg3_splice_array_content_prepends_seg3_block() {
        let original = serde_json::json!([
            {"type": "text", "text": "existing block 1"},
            {"type": "text", "text": "existing block 2"},
        ]);
        let folded = apply_seg3_fold(original, "seg3 memory");

        let arr = folded.as_array().expect("folded content must be an array");
        assert_eq!(arr.len(), 3, "seg3 prepended + 2 existing");

        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "seg3 memory");
        assert_eq!(arr[1]["text"], "existing block 1");
        assert_eq!(arr[2]["text"], "existing block 2");
    }

    /// When the original content is a structured JSON object (fallback case),
    /// it is stringified into a text block after the seg3 block.
    #[test]
    fn seg3_splice_object_content_stringifies_into_text_block() {
        let original = serde_json::json!({"result": 42});
        let folded = apply_seg3_fold(original, "seg3 memory");

        let arr = folded.as_array().expect("folded content must be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "seg3 memory");
        // The object is serialized to JSON string in the text field.
        let stringified = arr[1]["text"].as_str().expect("text field must be string");
        assert!(
            stringified.contains("42"),
            "stringified object must contain '42'"
        );
    }

    /// Regression guard: the splice MUST NOT flip ChatRole::Tool to ChatRole::User.
    ///
    /// This is the primary regression guard. If the role is ever flipped back
    /// to User, Anthropic will receive a user-role message with a text block
    /// PRECEDING the tool_result block, which violates the adjacency requirement
    /// and causes a 400. The role must stay Tool so the Anthropic adapter
    /// serializes it correctly as tool_result-in-user-message.
    #[test]
    fn seg3_splice_role_stays_tool_not_user() {
        use genai::chat::{ChatMessage, ChatRole, ContentPart, MessageContent, ToolResponse};

        // Construct a Tool-role message with one ToolResponse part.
        let tool_response = ToolResponse::new("toolu_01", "initial tool output");
        let original_msg = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(vec![ContentPart::ToolResponse(tool_response)]),
            options: None,
        };

        // Simulate the splice (inline, not via compose_request_for_turn which
        // requires full async SessionContext setup).
        let seg3_text = "seg3 memory context";
        let original_parts = original_msg.content.parts().clone();
        let mut new_parts: Vec<ContentPart> = Vec::with_capacity(original_parts.len());
        let mut folded = false;

        for part in original_parts.into_iter().rev() {
            if !folded && let ContentPart::ToolResponse(mut tr) = part {
                let seg3_block = serde_json::json!({"type": "text", "text": seg3_text});
                let folded_content = match tr.content {
                    serde_json::Value::String(ref s) => {
                        serde_json::json!([seg3_block, {"type": "text", "text": s}])
                    }
                    serde_json::Value::Array(ref items) => {
                        let mut arr = Vec::with_capacity(items.len() + 1);
                        arr.push(seg3_block);
                        arr.extend(items.iter().cloned());
                        serde_json::Value::Array(arr)
                    }
                    ref other => {
                        serde_json::json!([seg3_block, {"type": "text", "text": other.to_string()}])
                    }
                };
                tr.content = folded_content;
                new_parts.push(ContentPart::ToolResponse(tr));
                folded = true;
            } else {
                new_parts.push(part);
            }
        }
        new_parts.reverse();

        let mut result_msg = original_msg.clone();
        result_msg.content = MessageContent::from_parts(new_parts);
        // Role must NOT be flipped — this is the regression guard.
        result_msg.role = original_msg.role; // already Tool; explicit to make intent clear

        assert_eq!(
            result_msg.role,
            ChatRole::Tool,
            "role MUST remain Tool after splice — flipping to User causes Anthropic 400"
        );

        // Verify the content was actually folded.
        let parts = result_msg.content.parts();
        assert_eq!(parts.len(), 1, "still one ToolResponse part");
        let ContentPart::ToolResponse(ref tr) = parts[0] else {
            panic!("expected ToolResponse part");
        };
        let content_arr = tr
            .content
            .as_array()
            .expect("content must be array after fold");
        assert_eq!(content_arr.len(), 2, "seg3 block + original content block");
        assert_eq!(content_arr[0]["text"], "seg3 memory context");
        assert_eq!(content_arr[1]["text"], "initial tool output");
    }
}
