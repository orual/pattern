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
) -> Result<TurnOutput, RuntimeError> {
    // 1. Call the provider, consume the stream. Caller is responsible
    //    for having built `req` via the composer pipeline (segments
    //    1/2/3 + fresh input messages appended) — `orchestrate`
    //    itself doesn't know about the cache layout.
    let sink = ctx.turn_sink().clone();
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
    if has_segment_1 && cache_metrics.cache_read_input_tokens == 0 {
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

    loop {
        // Build the composed CompletionRequest for THIS wire turn:
        // segments 1 (system + persona + tools) / 2 (prior messages +
        // summary head + pseudo-messages) / 3 (current_state), then
        // fresh input messages appended AFTER compose so they stay
        // uncached (per the three-segment cache layout).
        let (req, has_segment_1) =
            compose_request_for_turn(&ctx, &turn_history, &cur_input, &cache_profile).await?;

        let turn = orchestrate(
            req,
            cur_input,
            ctx.clone(),
            dispatcher,
            preamble,
            has_segment_1,
        )
        .await?;
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

    // 4. Record whether segment 1 has content before `system_blocks`
    //    is moved into the pass. `build_system_prompt` always emits at
    //    least base-instructions, so this is almost always `true` — we
    //    track it explicitly so the AC8.5 bust warning has a reliable
    //    predicate rather than guessing.
    let has_segment_1 = !system_blocks.is_empty();

    // 4a. Assemble the pass list. Boxed so the compose() helper can
    //     iterate a uniform Vec<Box<dyn ComposerPass>>.
    let passes: Vec<Box<dyn ComposerPass>> = vec![
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
        // Segment 3: empty blocks today — see function doc for the
        // future-work note.
        Box::new(Segment3Pass::new(Vec::new(), cache_profile.clone())),
    ];

    let initial = PartialRequest::new(ctx.model_id());
    let mut req = compose(&passes, initial).map_err(|e| RuntimeError::ProviderError {
        reason: format!("composer pipeline failed: {e}"),
    })?;

    // 5. Append fresh input messages AFTER compose so they sit
    //    beyond the segment-3 cache boundary (uncached by design).
    for msg in &input.messages {
        req.chat.messages.push(msg.chat_message.clone());
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
        let out = orchestrate(simple_req(), test_turn_input(), ctx, &dispatcher, "", false)
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
            true, // has_segment_1 = true → bust warning expected
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
            true, // has_segment_1 = true, but cache_read > 0 → no warn
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
}
