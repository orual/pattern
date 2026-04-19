//! Agent-loop orchestrator — executes **one wire turn** end-to-end.
//!
//! A "wire turn" corresponds to one `ProviderClient::complete` call. One
//! user-visible exchange (`pattern_core::Session::step`) is driven by
//! the `TidepoolSession::step` method (from `crate::session`) as a loop over multiple wire turns,
//! chained via `TurnInput::continuation` when `stop_reason == ToolUse`.
//! This module implements the inner single-turn primitive.
//!
//! # Responsibilities
//!
//! 1. Build a `CompletionRequest` from the turn's `TurnInput` +
//!    [`crate::sdk::CODE_TOOL`] (full composer integration — segments
//!    1 / 2 / 3 — is wired in a follow-up change; this cut passes
//!    input messages through and injects the `code` tool).
//! 2. Stream the provider response, emitting `TurnEvent`s (from `pattern_core::traits`)
//!    to the session's `TurnSink` as events arrive: `Text` for LLM
//!    response chunks, `Thinking` for reasoning chunks, `ToolCall`
//!    when tool_use blocks complete, `ToolResult` after eval settles,
//!    `Stop` when the wire turn closes.
//! 3. Dispatch each captured tool_use to the provided `EvalDispatcher`
//!    after stream close, collect outcomes, and pair them by
//!    `call_id` into `ToolResult`s.
//! 4. Drain the memory adapter's pending `BlockWrite`s (from `pattern_runtime::memory`)
//!    and assemble a `TurnOutput` with: `messages` (including a reconstructed
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
use pattern_core::types::message::{
    Message, MessageAttachment, MidBatchDeltaBehavior, RenderedBlock, ResponseMeta, SnapshotKind,
};
use pattern_core::types::provider::{
    ChatMessage, ChatStreamEvent, CompletionRequest, ToolCall, ToolOutcome, ToolResult,
};
use pattern_core::types::turn::{StepReply, StopReason, TurnCacheMetrics, TurnInput, TurnOutput};

use pattern_provider::compose::passes::{Segment1Pass, Segment2Pass, synthesize_summary_message};
use pattern_provider::compose::{CacheProfile, ComposerPass, PartialRequest, compose};
use pattern_provider::shaper::{ShaperCompatMode, build_system_prompt, wrap_system_reminder};

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

    // 4b. Synthesise a single tool_result Message carrying ALL tool_result
    //     ContentPart::ToolResponse parts for this turn. Anthropic's wire
    //     format expects all tool_results from parallel dispatch in ONE
    //     user-role message; genai's ChatRole::Tool → user-role translation
    //     happens in the adapter. Inlining into TurnOutput.messages preserves
    //     the full round-trip in TurnHistory so the composer's Segment 2 pass
    //     replays [user, assistant(tool_use), tool_result, ...] correctly on
    //     subsequent wire turns.
    let tool_result_message: Option<Message> = if tool_results.is_empty() {
        None
    } else {
        use genai::chat::{ChatMessage, ChatRole, ContentPart, MessageContent};
        let parts: Vec<ContentPart> = tool_results
            .iter()
            .map(|r| ContentPart::from(r.to_tool_response()))
            .collect();
        let chat_msg = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(parts),
            options: Default::default(),
        };
        Some(Message {
            chat_message: chat_msg,
            id: MessageId::from(new_id()),
            position: pattern_core::types::ids::new_snowflake_id(),
            owner_id: AgentId::from(ctx.agent_id()),
            created_at: Timestamp::now(),
            batch: input.batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        })
    };

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

    // Assemble messages in wire order: assistant message first (if any),
    // then the tool_result message (if this was a tool-use turn). This
    // preserves the complete round-trip in TurnHistory so the composer's
    // Segment 2 pass replays [assistant(tool_use), tool_result] correctly.
    let messages = {
        let mut v = Vec::with_capacity(2);
        if let Some(m) = assistant_message {
            v.push(m);
        }
        if let Some(m) = tool_result_message {
            v.push(m);
        }
        v
    };

    Ok(TurnOutput {
        messages,
        block_writes,
        tool_calls,
        stop_reason,
        usage,
        cache_metrics,
        completed_at: Timestamp::now(),
    })
}

// ---- snapshot builder ---------------------------------------------------

/// Stable content hash of rendered text for delta comparison.
///
/// Uses blake3 (purpose-built non-crypto hash: stable across Rust
/// compiler versions, platforms, and process restarts) and truncates
/// the 32-byte digest to a u64 for storage. Collision probability at
/// our scale (tens of blocks across tens of snapshots per session) is
/// negligible — birthday attack for u64 requires ~2^32 ≈ 4 billion
/// entries before a 50% collision chance.
///
/// Cross-version stability isn't strictly needed today (active
/// TurnHistory turns don't persist across process restarts — see
/// `TurnHistory::load` which always starts `active` empty), but the
/// blake3 output is stable so if active-turn persistence lands later,
/// resumed sessions will still recognize prior-turn hashes correctly.
fn content_hash(text: &str) -> u64 {
    let digest = blake3::hash(text.as_bytes());
    let bytes = digest.as_bytes();
    // Truncate to u64 via little-endian first 8 bytes.
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Render a block into the `<block:label ...>` tagged format, producing
/// a [`RenderedBlock`] with visibility determined by the `visible`
/// parameter.
///
/// Freezes the live `StructuredDocument` content into an owned
/// `Arc<str>` snapshot when `visible` is true. When false, the block
/// is tracked (hash present) but rendered content is `None`.
fn render_block_for_snapshot(block: &StructuredDocument, visible: bool) -> RenderedBlock {
    let label = smol_str::SmolStr::new(block.label());
    let bt = block.block_type();
    let block_type_str = match bt {
        pattern_core::memory::BlockType::Core => "core",
        pattern_core::memory::BlockType::Working => "working",
        pattern_core::memory::BlockType::Archival => "archival",
        pattern_core::memory::BlockType::Log => "log",
    };
    let permission = block.permission().to_string();
    let content = block.render();
    let description = block.description();

    let open_tag = format!("<block:{label} type=\"{block_type_str}\" permission=\"{permission}\">");
    let close_tag = format!("</block:{label}>");
    let inner = if description.is_empty() {
        content
    } else {
        format!("{description}\n\n{content}")
    };
    let rendered_str = format!("{open_tag}\n{inner}\n{close_tag}");
    let hash = content_hash(&rendered_str);

    RenderedBlock {
        label,
        block_type: bt,
        rendered: if visible {
            Some(std::sync::Arc::from(rendered_str.as_str()))
        } else {
            None
        },
        content_hash: hash,
    }
}

/// Build a [`MessageAttachment::BatchOpeningSnapshot`] from the current
/// memory blocks and the prior snapshot (if any, for delta computation).
///
/// For `SnapshotKind::Full`: bundles all blocks.
/// For `SnapshotKind::Delta`: includes only blocks whose content hash
/// differs from the prior snapshot.
fn build_snapshot_attachment(
    kind: SnapshotKind,
    current_blocks: Vec<RenderedBlock>,
    prior_tracked_hashes: Option<std::collections::HashMap<String, u64>>,
) -> MessageAttachment {
    let block_names: Vec<smol_str::SmolStr> =
        current_blocks.iter().map(|b| b.label.clone()).collect();

    match &kind {
        SnapshotKind::Full => MessageAttachment::BatchOpeningSnapshot {
            kind,
            block_names,
            blocks: current_blocks,
            edited_blocks: vec![],
        },
        SnapshotKind::Delta { .. } => {
            // Use the pre-collected prior_hashes map. This map walks the
            // FULL history (latest-wins per label), not just the most-recent
            // attachment — critical because an immediate-prior Delta with
            // empty `blocks` would otherwise leave prior_hashes empty and
            // cause every current block to spuriously appear "new or changed".
            let prior_hashes = prior_tracked_hashes.unwrap_or_default();

            let mut edited_blocks = Vec::new();
            let mut delta_blocks = Vec::new();

            for block in &current_blocks {
                let is_new_or_changed = prior_hashes
                    .get(block.label.as_str())
                    .map(|&prev_hash| prev_hash != block.content_hash)
                    .unwrap_or(true); // truly new block (never seen) = include
                if is_new_or_changed {
                    edited_blocks.push(block.label.clone());
                    delta_blocks.push(block.clone());
                }
            }

            MessageAttachment::BatchOpeningSnapshot {
                kind,
                block_names,
                blocks: delta_blocks,
                edited_blocks,
            }
        }
    }
}

/// Render a [`MessageAttachment::BatchOpeningSnapshot`] into a
/// `<system-reminder>`-wrapped text block for compose-time splicing.
fn render_snapshot_attachment(attachment: &MessageAttachment) -> String {
    let MessageAttachment::BatchOpeningSnapshot {
        kind,
        block_names,
        blocks,
        edited_blocks,
    } = attachment;

    let mut parts = Vec::new();

    // Header.
    parts.push("[memory:current_state]".to_string());

    // Kind indicator.
    match kind {
        SnapshotKind::Full => {
            parts.push("(full snapshot)".to_string());
        }
        SnapshotKind::Delta { since_batch } => {
            parts.push(format!("(delta since batch {since_batch})"));
            if !edited_blocks.is_empty() {
                let names: Vec<&str> = edited_blocks.iter().map(|s| s.as_str()).collect();
                parts.push(format!(
                    "[memory:updated] blocks changed: {}",
                    names.join(", ")
                ));
            }
        }
    }

    // Block namespace.
    if block_names.is_empty() {
        parts.push("(no blocks loaded)".to_string());
    } else {
        let names: Vec<&str> = block_names.iter().map(|s| s.as_str()).collect();
        parts.push(format!("Available blocks: {}", names.join(", ")));
    }

    // Block contents (only render visible blocks).
    for block in blocks {
        if let Some(ref rendered) = block.rendered {
            parts.push(rendered.to_string());
        }
    }

    let body = parts.join("\n\n");
    wrap_system_reminder(&body)
}

/// Collect the most recent rendered content hash for each block label
/// from the turn history's attachments. Used to determine "last shown"
/// state for the visibility decision. Call while holding the history
/// lock; the result is a map from label -> hash.
fn collect_last_shown_hashes(history: &TurnHistory) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    for record in history.iter_active().rev() {
        // Check output then input messages (most recent first).
        let all_msgs = record
            .output
            .messages
            .iter()
            .rev()
            .chain(record.input.messages.iter().rev());
        for msg in all_msgs {
            for att in &msg.attachments {
                let MessageAttachment::BatchOpeningSnapshot { blocks, .. } = att;
                for bs in blocks {
                    if bs.rendered.is_some() && !map.contains_key(bs.label.as_str()) {
                        map.insert(bs.label.to_string(), bs.content_hash);
                    }
                }
            }
        }
    }
    map
}

/// Walk prior attachments and build a label -> content_hash map of the
/// most-recent TRACKED hash per label (including blocks whose rendering
/// was suppressed via the visibility gate). Latest-wins per label.
///
/// Used by `build_snapshot_attachment` to detect which blocks changed
/// since they were last tracked. Distinct from `collect_last_shown_hashes`,
/// which filters to only rendered entries (for the visibility-gating
/// decision of whether to surface a changed block's content inline).
fn collect_last_tracked_hashes(history: &TurnHistory) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    for record in history.iter_active().rev() {
        let all_msgs = record
            .output
            .messages
            .iter()
            .rev()
            .chain(record.input.messages.iter().rev());
        for msg in all_msgs {
            for att in &msg.attachments {
                let MessageAttachment::BatchOpeningSnapshot { blocks, .. } = att;
                for bs in blocks {
                    // Track EVERY block regardless of rendering — the hash
                    // is present for delta detection even when rendered=None.
                    if !map.contains_key(bs.label.as_str()) {
                        map.insert(bs.label.to_string(), bs.content_hash);
                    }
                }
            }
        }
    }
    map
}

/// Load memory blocks for snapshot construction, filtered by the given
/// [`SnapshotSelection`] policy. Persona is always excluded (it lives
/// in segment 1's system prompt).
///
/// Block visibility (rendered vs tracked-but-silent) is determined by:
/// - **Core** blocks: always visible.
/// - **Working** blocks: visible when pinned or block_ref'd, AND content
///   changed since last shown (or never shown). Otherwise tracked-but-silent.
///
/// `shown_hashes` maps block label -> last rendered content hash (from
/// [`collect_last_shown_hashes`]).
async fn load_snapshot_blocks_with_visibility(
    ctx: &SessionContext,
    kind: &SnapshotKind,
    selection: &pattern_core::types::message::SnapshotSelection,
    block_refs: &[pattern_core::types::block_ref::BlockRef],
    shown_hashes: &std::collections::HashMap<String, u64>,
) -> Result<Vec<RenderedBlock>, RuntimeError> {
    let block_list = ctx
        .memory_store()
        .list_blocks(ctx.agent_id())
        .await
        .map_err(|e| RuntimeError::ProviderError {
            reason: format!("list_blocks failed: {e}"),
        })?;
    let is_full = matches!(kind, SnapshotKind::Full);
    let mut blocks = Vec::new();
    for meta in block_list {
        // Persona lives in segment 1 (system prompt); don't duplicate its
        // content in segment 3. Still include its LABEL in the snapshot
        // (as rendered=None) so the model sees the full block namespace
        // and future delta checks can detect persona edits.
        let is_persona = meta.label == pattern_core::PERSONA_LABEL;
        if !is_persona && !selection.accepts(&meta.label, meta.block_type) {
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
            // Always render to get the content hash for tracking.
            let rendered = render_block_for_snapshot(&doc, true);

            // Persona is NEVER rendered inline (already in segment 1).
            // Otherwise: Full always renders everything; Delta applies
            // the pinned/block_refs visibility gate for Working blocks.
            let visible = if is_persona {
                false
            } else if is_full {
                true
            } else {
                block_visibility_from_hashes(&doc, block_refs, shown_hashes, rendered.content_hash)
            };

            if visible {
                blocks.push(rendered);
            } else {
                blocks.push(RenderedBlock {
                    rendered: None,
                    ..rendered
                });
            }
        }
    }
    Ok(blocks)
}

/// Determine block visibility from pre-collected shown hashes.
/// Same logic as `block_visibility` but without requiring TurnHistory.
fn block_visibility_from_hashes(
    block: &StructuredDocument,
    block_refs: &[pattern_core::types::block_ref::BlockRef],
    shown_hashes: &std::collections::HashMap<String, u64>,
    current_hash: u64,
) -> bool {
    use pattern_core::memory::BlockType;
    match block.block_type() {
        BlockType::Core => true,
        BlockType::Working => {
            let label = block.label();
            let is_pinned = block.is_pinned();
            let is_refd = block_refs.iter().any(|r| r.label.as_str() == label);
            if is_pinned || is_refd {
                // Visible unless unchanged since last shown.
                !matches!(shown_hashes.get(label), Some(&prev) if prev == current_hash)
            } else {
                false
            }
        }
        BlockType::Archival | BlockType::Log => false,
    }
}

// ---- message persistence ------------------------------------------------

/// Map a `genai::chat::ChatRole` to the corresponding
/// `pattern_db::models::MessageRole` for storage.
fn map_chat_role(role: genai::chat::ChatRole) -> pattern_db::models::MessageRole {
    match role {
        genai::chat::ChatRole::User => pattern_db::models::MessageRole::User,
        genai::chat::ChatRole::Assistant => pattern_db::models::MessageRole::Assistant,
        genai::chat::ChatRole::System => pattern_db::models::MessageRole::System,
        genai::chat::ChatRole::Tool => pattern_db::models::MessageRole::Tool,
    }
}

/// Infer a `pattern_db::models::BatchType` from a `MessageOrigin`.
///
/// Mapping:
/// - Partner / Human author → `UserRequest`.
/// - Agent author → `AgentToAgent`.
/// - System author with `ToolCall` reason → `Continuation`.
/// - System author with any other reason → `SystemTrigger`.
fn infer_batch_type(
    origin: &pattern_core::types::origin::MessageOrigin,
) -> pattern_db::models::BatchType {
    use pattern_core::types::origin::{Author, SystemReason};
    match &origin.author {
        Author::Partner(_) | Author::Human(_) => pattern_db::models::BatchType::UserRequest,
        Author::Agent(_) => pattern_db::models::BatchType::AgentToAgent,
        Author::System { reason } => match reason {
            SystemReason::ToolCall => pattern_db::models::BatchType::Continuation,
            _ => pattern_db::models::BatchType::SystemTrigger,
        },
        // Author is #[non_exhaustive]; future variants default to UserRequest.
        _ => pattern_db::models::BatchType::UserRequest,
    }
}

/// Extract a plaintext preview from a `ChatMessage`, truncated to ~200 chars.
fn content_preview(msg: &genai::chat::ChatMessage) -> Option<String> {
    let text = msg.content.joined_texts()?;
    if text.len() <= 200 {
        Some(text)
    } else {
        // Truncate to 200 chars (byte-safe via char boundary).
        let boundary = text
            .char_indices()
            .nth(200)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        Some(format!("{}…", &text[..boundary]))
    }
}

/// Convert a `pattern_core::Message` to a `pattern_db::models::Message` for
/// storage.
///
/// Attachments are intentionally dropped: pattern_db has no attachment column,
/// and per Phase 6 design, next session rebuilds snapshots from memory_blocks.
/// Losing them on the DB path is acceptable.
fn to_db_message(
    msg: &Message,
    agent_id: &str,
    batch_type: pattern_db::models::BatchType,
) -> Result<pattern_db::models::Message, RuntimeError> {
    let content_json = serde_json::to_value(&msg.chat_message).map_err(|e| {
        RuntimeError::DatabasePersistenceFailed {
            step: "serialize chat_message".into(),
            reason: e.to_string(),
        }
    })?;

    // Convert jiff::Timestamp → chrono::DateTime<Utc>.
    let epoch_nanos = msg.created_at.as_nanosecond();
    let secs = (epoch_nanos / 1_000_000_000) as i64;
    let nanos = (epoch_nanos % 1_000_000_000) as u32;
    let created_at = chrono::DateTime::from_timestamp(secs, nanos).unwrap_or_else(chrono::Utc::now);

    Ok(pattern_db::models::Message {
        id: msg.id.to_string(),
        agent_id: agent_id.to_string(),
        position: msg.position.to_string(),
        batch_id: Some(msg.batch.to_string()),
        sequence_in_batch: None, // position handles ordering; within-batch sequence is redundant.
        role: map_chat_role(msg.chat_message.role.clone()),
        content_json: pattern_db::Json(content_json),
        content_preview: content_preview(&msg.chat_message),
        batch_type: Some(batch_type),
        source: None,
        source_metadata: None,
        is_archived: false,
        is_deleted: false,
        created_at,
    })
}

/// Persist a slice of `pattern_core::Message`s to pattern_db via upsert.
///
/// Uses `upsert_message` for idempotency: if the same message would be
/// inserted twice (e.g. restart + replay), the UNIQUE constraint on `id`
/// is handled gracefully via ON CONFLICT DO UPDATE.
async fn persist_messages(
    db: &pattern_db::ConstellationDb,
    messages: &[Message],
    agent_id: &str,
    batch_type: pattern_db::models::BatchType,
    step_label: &str,
) -> Result<(), RuntimeError> {
    for msg in messages {
        let db_msg = to_db_message(msg, agent_id, batch_type)?;
        pattern_db::queries::upsert_message(db.pool(), &db_msg)
            .await
            .map_err(|e| RuntimeError::DatabasePersistenceFailed {
                step: step_label.to_string(),
                reason: e.to_string(),
            })?;
    }
    Ok(())
}

// ---- drive_step — loop driver -------------------------------------------

/// Drive one user-visible exchange: repeatedly call `orchestrate`
/// until `stop_reason.is_terminal()`, recording each turn's full
/// round-trip (input + output) to `TurnHistory` and threading
/// continuation turns via `TurnInput::continuation`.
///
/// Called by `crate::session::TidepoolSession::step` as the main
/// user-visible entry point. Preserves `batch_id` across all wire
/// turns; mints a fresh `turn_id` per wire turn (via
/// `TurnInput::continuation`).
///
/// Returns a `StepReply` aggregating every wire turn's
/// `TurnOutput`.
///
/// [`TurnHistory`]: crate::memory::TurnHistory
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

    // ---- Attach batch-opening snapshot to the first user message ----
    //
    // This is a new batch (drive_step = one batch). Build a snapshot
    // attachment and attach it to the first user message in the input.
    // Continuation turns within this batch don't get new attachments
    // — the batch-opening attachment is already in history.
    if !cur_input.messages.is_empty() {
        // Determine snapshot kind.
        let (snapshot_kind, prior_tracked_hashes) = {
            let hist = turn_history
                .lock()
                .map_err(|_| RuntimeError::ProviderError {
                    reason: "turn_history mutex poisoned".into(),
                })?;

            let kind = if hist.active_len() == 0
                || hist.post_compaction_pending()
                || hist.batches_since_last_full() >= 10
            {
                SnapshotKind::Full
            } else {
                SnapshotKind::Delta {
                    since_batch: hist
                        .most_recent_batch_id()
                        .cloned()
                        .unwrap_or_else(|| batch_id.clone()),
                }
            };

            // For Delta, walk FULL history for a latest-wins per-label hash
            // map. Using just `find_prior_snapshot` would regress when the
            // most-recent attachment was itself an empty Delta — leaving
            // prior_hashes empty and making every block appear new.
            let prior_hashes = if matches!(kind, SnapshotKind::Delta { .. }) {
                Some(collect_last_tracked_hashes(&hist))
            } else {
                None
            };

            (kind, prior_hashes)
        };

        // Fetch current memory blocks, filtered by snapshot selection
        // policy. Persona is always excluded (lives in seg1).
        // Extract the "last shown" hash map from history while holding
        // the lock briefly, then release before async calls.
        let selection = ctx.snapshot_selection().clone();
        let first_msg_block_refs: Vec<pattern_core::types::block_ref::BlockRef> = cur_input
            .messages
            .first()
            .map(|m| m.block_refs.clone())
            .unwrap_or_default();
        let shown_hashes: std::collections::HashMap<String, u64> = turn_history
            .lock()
            .map(|h| collect_last_shown_hashes(&h))
            .unwrap_or_default();
        let current_blocks = load_snapshot_blocks_with_visibility(
            &ctx,
            &snapshot_kind,
            &selection,
            &first_msg_block_refs,
            &shown_hashes,
        )
        .await?;

        let is_full = matches!(snapshot_kind, SnapshotKind::Full);
        let attachment =
            build_snapshot_attachment(snapshot_kind, current_blocks, prior_tracked_hashes);

        // Attach to the first user message.
        if let Some(first_msg) = cur_input.messages.first_mut() {
            first_msg.attachments.push(attachment);
        }

        // Update TurnHistory snapshot tracking.
        if is_full && let Ok(mut hist) = turn_history.lock() {
            hist.note_full_snapshot_emitted();
        }
    }

    loop {
        // Compaction gate: check whether the active context needs
        // compression BEFORE composing the request. This ensures
        // archived turns are removed from TurnHistory before the
        // composer reads it for segment 2.
        let compaction_outcome =
            crate::compaction::maybe_compact(&ctx, &turn_history, ctx.context_policy()).await?;
        tracing::debug!(?compaction_outcome, "compaction check");

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

        // Clone cur_input before moving it into orchestrate so we can pass it
        // to hist.record after orchestrate completes. orchestrate takes
        // ownership of TurnInput (it reads batch_id from it during the turn).
        let recorded_input = cur_input.clone();

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

        // ---- Mid-batch delta attachment ----
        //
        // On non-terminal turns (tool_use), check if memory state has
        // changed since the last attachment in this batch. If external
        // actors or tool execution mutated memory, attach a Delta to the
        // tool_result message so the model sees the changes on the next
        // wire turn. Intra-step cache churn is acceptable (steps are
        // short, TTL is longer).
        let mut turn = turn;
        if !terminal && !turn.messages.is_empty() {
            // Find the tool_result message (last message with Role::Tool).
            let tool_msg_idx = turn
                .messages
                .iter()
                .rposition(|m| m.chat_message.role == genai::chat::ChatRole::Tool);

            if let Some(idx) = tool_msg_idx {
                // Fetch current memory blocks (filtered by selection).
                // For mid-batch deltas, use the tool_result message's
                // block_refs for visibility decisions.
                let tool_block_refs = turn.messages[idx].block_refs.clone();
                let mid_shown_hashes: std::collections::HashMap<String, u64> = turn_history
                    .lock()
                    .map(|h| collect_last_shown_hashes(&h))
                    .unwrap_or_default();
                // Mid-batch snapshots are always Delta (the batch-opening
                // Full was already attached on the batch's user message).
                // Use the most recent BatchId as the delta baseline; this
                // parameter isn't read by the visibility logic (only by
                // build_snapshot_attachment's delta diff), but we include
                // it for consistency.
                let mid_kind = SnapshotKind::Delta {
                    since_batch: recorded_input.batch_id.clone(),
                };
                if let Ok(current_blocks) = load_snapshot_blocks_with_visibility(
                    &ctx,
                    &mid_kind,
                    ctx.snapshot_selection(),
                    &tool_block_refs,
                    &mid_shown_hashes,
                )
                .await
                {
                    // Build the prior-tracked-hashes map by walking FULL
                    // turn_history (latest-wins per label) and then folding
                    // in any attachments from recorded_input that haven't
                    // been pushed to history yet (this wire turn's input).
                    let mut prior_hashes: std::collections::HashMap<String, u64> = turn_history
                        .lock()
                        .map(|h| collect_last_tracked_hashes(&h))
                        .unwrap_or_default();
                    for msg in &recorded_input.messages {
                        for att in &msg.attachments {
                            let MessageAttachment::BatchOpeningSnapshot { blocks, .. } = att;
                            for bs in blocks {
                                // recorded_input is MORE recent than history,
                                // so it overwrites.
                                prior_hashes.insert(bs.label.to_string(), bs.content_hash);
                            }
                        }
                    }

                    // Under FilterSelfEdits, exclude block labels that this
                    // turn's own tool calls wrote. The agent already saw
                    // those writes via its tool_result content; re-attaching
                    // them as a delta is redundant cache churn. Under
                    // IncludeSelfEdits (the default), the set is empty and
                    // every changed block triggers a delta.
                    let self_written: std::collections::HashSet<&str> = if matches!(
                        ctx.snapshot_policy().mid_batch,
                        MidBatchDeltaBehavior::FilterSelfEdits
                    ) {
                        turn.block_writes
                            .iter()
                            .map(|bw| bw.handle.as_str())
                            .collect()
                    } else {
                        std::collections::HashSet::new()
                    };

                    // Check if any EXTERNAL blocks changed vs the walked prior.
                    let has_external_changes = current_blocks.iter().any(|b| {
                        let label = b.label.as_str();
                        if self_written.contains(label) {
                            return false; // self-edit, already visible via tool_result
                        }
                        prior_hashes
                            .get(label)
                            .map(|&h| h != b.content_hash)
                            .unwrap_or(true) // truly new block
                    });

                    if has_external_changes {
                        let delta = build_snapshot_attachment(
                            SnapshotKind::Delta {
                                since_batch: batch_id.clone(),
                            },
                            current_blocks,
                            Some(prior_hashes),
                        );
                        turn.messages[idx].attachments.push(delta);
                        tracing::debug!(
                            agent_id = ctx.agent_id(),
                            "mid-batch delta attached to tool_result message"
                        );
                    }
                }
            }
        }

        // Record into TurnHistory so the NEXT wire turn's composer sees this
        // turn's full round-trip (input + output) in Segment 2.
        // Use new_snowflake_id() for the TurnId: snowflake IDs are time-ordered
        // and globally unique, consistent with all other TurnId minting in the
        // runtime. new_id() (UUID-v4) is reserved for MessageId / non-ordered IDs.
        if let Ok(mut hist) = turn_history.lock() {
            hist.record(
                pattern_core::types::ids::new_snowflake_id(),
                recorded_input.clone(),
                turn.clone(),
            );
        }

        // ---- Persist messages to pattern_db ----
        //
        // Upsert every input + output message so the messages table has
        // actual rows for compression to archive. Attachments are
        // intentionally dropped (pattern_db has no attachment column;
        // next session rebuilds snapshots from memory_blocks).
        let batch_type = infer_batch_type(&recorded_input.origin);
        let db = ctx.db();
        let aid = ctx.agent_id();

        // Input messages (from the caller's TurnInput).
        persist_messages(
            db,
            &recorded_input.messages,
            aid,
            batch_type,
            "upsert input messages",
        )
        .await?;

        // Output messages (assistant reply + optional tool_result).
        persist_messages(
            db,
            &turn.messages,
            aid,
            batch_type,
            "upsert output messages",
        )
        .await?;

        turns.push(turn);

        if terminal {
            break;
        }

        // Build the next wire turn's continuation input. The tool_result
        // messages from THIS turn have been recorded into history via
        // hist.record above, so the continuation input contributes no fresh
        // messages — it just carries the batch_id forward and mints a fresh
        // turn_id. The composer's Segment 2 pass replays the prior turn's
        // [assistant(tool_use), tool_result] from history.
        cur_input = TurnInput::continuation(batch_id.clone(), agent_id.clone());
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
/// Runs the two-segment composer pipeline plus attachment splice:
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
/// - **Segment 3 (attachment splice)** — memory snapshots are attached
///   to batch-opening user messages (and optionally to mid-batch
///   tool_result messages when external memory changes are detected)
///   as `MessageAttachment::BatchOpeningSnapshot`. These are spliced
///   onto the corresponding ChatMessages at compose-time, producing
///   `<system-reminder>`-wrapped content. The segment-3 cache boundary
///   is placed on the last message with a spliced attachment.
///
/// This architecture keeps historical messages' wire content stable
/// across turns (the attachment is frozen at Message creation time),
/// enabling better cache hit rates than the old approach of splicing
/// into the "last message" which changed identity between turns.
///
/// Fresh `input.messages` are appended AFTER `compose` returns, so
/// they sit past the segment-2 cache boundary (stay uncached — fresh
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
    //    doc for the rationale. Persona's optional system_prompt
    //    replaces DEFAULT_BASE_INSTRUCTIONS in slot[1] when set.
    let mode = default_shaper_mode();
    let base_instructions = ctx
        .system_prompt()
        .unwrap_or(pattern_core::DEFAULT_BASE_INSTRUCTIONS);
    let system_blocks = build_system_prompt(mode, base_instructions, &persona_text, &[]);

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
    //    is moved into the pass.
    let has_segment_1 = !system_blocks.is_empty();

    // 5. Assemble the composer pass list: Segment 1 + Segment 2.
    //    Segment 3 is NO LONGER a separate composer pass — memory
    //    snapshots are now attached to batch-opening user messages as
    //    `MessageAttachment::BatchOpeningSnapshot` and spliced onto
    //    the wire at compose-time (step 8 below). This eliminates
    //    the cache-busting problem where the old seg3 pseudo-message
    //    changed the "last message" identity across turns.
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
    ];

    let initial = PartialRequest::new(ctx.model_id());
    let mut req = compose(&passes, initial).map_err(|e| RuntimeError::ProviderError {
        reason: format!("composer pipeline failed: {e}"),
    })?;

    // 6. Start from the persona's declared chat_options (temperature,
    //    max_tokens, top_p, reasoning_effort, verbosity, seed,
    //    stop_sequences, cache_control, prompt_cache_key, etc.) and layer
    //    on streaming-capture flags. Composer previously started from
    //    ChatOptions::default() which silently dropped all
    //    persona-declared sampling knobs.
    req.options = ctx
        .chat_options()
        .clone()
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_capture_reasoning_content(true);

    // 7. Append fresh input messages AFTER compose so they sit
    //    beyond the cache boundary (uncached by design).
    for msg in &input.messages {
        req.chat.messages.push(msg.chat_message.clone());
    }

    // 8. Splice attachment content onto the composed request.
    //
    //    Walk ALL pattern-level Messages that contributed to this request
    //    (both from history via Segment2Pass and from fresh input). For
    //    each message with non-empty attachments, render the attachment
    //    and splice it onto the corresponding ChatMessage in the composed
    //    request.
    //
    //    History messages were added by Segment2Pass as plain ChatMessages
    //    (no attachments — those live on the Pattern Message). We need to
    //    find the corresponding ChatMessage in the composed request for
    //    each history message that has attachments, and splice there.
    //
    //    Strategy: walk the history messages in order and match them to
    //    composed messages by content identity (same ChatMessage reference).
    //    For fresh input messages, they were just appended above — their
    //    position is known.
    //
    //    Simpler approach: since attachments are only on batch-opening
    //    user messages, we look for them in:
    //    (a) History messages from Segment2Pass — these appear as
    //        ChatMessages in the composed request. We need to find them.
    //    (b) Fresh input messages — these were just appended.
    //
    //    For (a), we walk the history and track which composed message
    //    index each history message maps to. For (b), fresh messages are
    //    at known indices: composed_len_after_seg2 .. composed_len_after_seg2 + input.messages.len().

    let num_fresh = input.messages.len();
    let total_composed = req.chat.messages.len();
    let seg2_end = total_composed - num_fresh; // index range [0..seg2_end) is from composer

    // Splice attachments from fresh input messages.
    // Fresh messages are at indices [seg2_end..total_composed).
    let mut last_spliced_idx: Option<usize> = None;
    for (i, msg) in input.messages.iter().enumerate() {
        let composed_idx = seg2_end + i;
        for attachment in &msg.attachments {
            let rendered = render_snapshot_attachment(attachment);
            // Append as a new ContentPart::Text after existing content.
            splice_text_onto_message(&mut req.chat.messages[composed_idx], &rendered);
            last_spliced_idx = Some(composed_idx);
        }
    }

    // Splice attachments from history messages (Segment2Pass output).
    // History messages were collected via active_messages() which yields
    // them in order. Segment2Pass pushes summary_head messages first,
    // then prior_messages, then pseudo-messages (block writes). The
    // prior_messages correspond 1:1 to active_messages() in order.
    // We need to find the offset where prior_messages start in the
    // composed request.
    {
        let hist = turn_history
            .lock()
            .map_err(|_| RuntimeError::ProviderError {
                reason: "turn_history mutex poisoned".into(),
            })?;

        // Count summary head messages that were prepended.
        let summary_count = hist.summary_head().len();

        // Prior messages start at index summary_count in the composed
        // message list (after summary head messages, before pseudo-messages).
        // Pseudo-messages (block writes) come AFTER prior_messages and are
        // not indexed here — attachment splice only targets prior_messages.
        let prior_start = summary_count;

        for (i, msg) in hist.active_messages().enumerate() {
            if msg.attachments.is_empty() {
                continue;
            }
            let composed_idx = prior_start + i;
            if composed_idx >= seg2_end {
                // Out of range for Segment2Pass — skip (shouldn't happen).
                continue;
            }
            for attachment in &msg.attachments {
                let rendered = render_snapshot_attachment(attachment);
                splice_text_onto_message(&mut req.chat.messages[composed_idx], &rendered);
                last_spliced_idx = Some(composed_idx);
            }
        }
    }

    // 9. Place cache_control marker on the LAST message that had an
    //    attachment spliced (the new seg3 boundary). If no attachments
    //    were spliced (continuation turn with no fresh input), fall
    //    through — the seg2 marker is the last cache boundary.
    if let Some(idx) = last_spliced_idx {
        let opts = req.chat.messages[idx]
            .options
            .clone()
            .unwrap_or_default()
            .with_cache_control(cache_profile.segment_3_control());
        req.chat.messages[idx].options = Some(opts);
    }

    Ok((req, has_segment_1))
}

/// Splice rendered text onto a `ChatMessage`'s content.
///
/// For user-role messages: appends as a `ContentPart::Text` AFTER existing
/// content. For tool-role messages: folds into the LAST `ToolResponse`'s
/// content array (same as the old `smooshIntoToolResult` pattern), preserving
/// Anthropic's wire-format constraint that `tool_result` blocks come first.
fn splice_text_onto_message(msg: &mut ChatMessage, text: &str) {
    use genai::chat::{ChatRole, ContentPart, MessageContent};

    match msg.role {
        ChatRole::Tool => {
            // Fold into the last ToolResponse's content array.
            let original_parts = msg.content.parts().clone();
            let mut new_parts: Vec<ContentPart> = Vec::with_capacity(original_parts.len());
            let mut folded = false;

            for part in original_parts.into_iter().rev() {
                if !folded && let ContentPart::ToolResponse(mut tr) = part {
                    let seg3_block = serde_json::json!({"type": "text", "text": text});
                    let folded_content = match tr.content {
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
                    };
                    tr.content = folded_content;
                    new_parts.push(ContentPart::ToolResponse(tr));
                    folded = true;
                    continue;
                }
                new_parts.push(part);
            }
            new_parts.reverse();
            msg.content = MessageContent::from_parts(new_parts);
        }
        _ => {
            // User, Assistant, System: append as text part.
            let mut parts = msg.content.parts().clone();
            parts.push(ContentPart::Text(text.to_string()));
            msg.content = MessageContent::from_parts(parts);
        }
    }
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
        position: pattern_core::types::ids::new_snowflake_id(),
        owner_id: AgentId::from(agent_id),
        created_at: Timestamp::now(),
        batch: batch_id,
        response_meta,
        block_refs: vec![],
        attachments: vec![],
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
///
/// `prompt_tokens_details` and `completion_tokens_details` are summed
/// field-by-field rather than `.or()`-ing, because each wire turn
/// contributes its own cache hits/creations. Discarding the second
/// turn's details would undercount cross-turn cache activity.
fn merge_usage(a: genai::chat::Usage, b: genai::chat::Usage) -> genai::chat::Usage {
    use genai::chat::Usage;
    Usage {
        prompt_tokens: sum_opt(a.prompt_tokens, b.prompt_tokens),
        completion_tokens: sum_opt(a.completion_tokens, b.completion_tokens),
        total_tokens: sum_opt(a.total_tokens, b.total_tokens),
        prompt_tokens_details: merge_prompt_tokens_details(
            a.prompt_tokens_details,
            b.prompt_tokens_details,
        ),
        completion_tokens_details: merge_completion_tokens_details(
            a.completion_tokens_details,
            b.completion_tokens_details,
        ),
    }
}

/// Sum two `PromptTokensDetails` values field-by-field. All numeric fields
/// are additive across wire turns (each turn has its own cache hits /
/// creations). `cache_creation_details` is also summed if both are present.
fn merge_prompt_tokens_details(
    a: Option<genai::chat::PromptTokensDetails>,
    b: Option<genai::chat::PromptTokensDetails>,
) -> Option<genai::chat::PromptTokensDetails> {
    use genai::chat::{CacheCreationDetails, PromptTokensDetails};
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => {
            let cache_creation_details = match (x.cache_creation_details, y.cache_creation_details)
            {
                (None, None) => None,
                (Some(d), None) => Some(d),
                (None, Some(d)) => Some(d),
                (Some(dx), Some(dy)) => Some(CacheCreationDetails {
                    ephemeral_5m_tokens: sum_opt(dx.ephemeral_5m_tokens, dy.ephemeral_5m_tokens),
                    ephemeral_1h_tokens: sum_opt(dx.ephemeral_1h_tokens, dy.ephemeral_1h_tokens),
                }),
            };
            Some(PromptTokensDetails {
                cache_creation_tokens: sum_opt(x.cache_creation_tokens, y.cache_creation_tokens),
                cache_creation_details,
                cached_tokens: sum_opt(x.cached_tokens, y.cached_tokens),
                audio_tokens: sum_opt(x.audio_tokens, y.audio_tokens),
            })
        }
    }
}

/// Sum two `CompletionTokensDetails` values field-by-field. All numeric
/// fields are additive across wire turns.
fn merge_completion_tokens_details(
    a: Option<genai::chat::CompletionTokensDetails>,
    b: Option<genai::chat::CompletionTokensDetails>,
) -> Option<genai::chat::CompletionTokensDetails> {
    use genai::chat::CompletionTokensDetails;
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(CompletionTokensDetails {
            accepted_prediction_tokens: sum_opt(
                x.accepted_prediction_tokens,
                y.accepted_prediction_tokens,
            ),
            rejected_prediction_tokens: sum_opt(
                x.rejected_prediction_tokens,
                y.rejected_prediction_tokens,
            ),
            reasoning_tokens: sum_opt(x.reasoning_tokens, y.reasoning_tokens),
            audio_tokens: sum_opt(x.audio_tokens, y.audio_tokens),
        }),
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
            thought_signatures_provenance: None,
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

    /// `merge_usage` sums `prompt_tokens_details` field-by-field rather than
    /// discarding the second turn's data via `.or()`. Both cache-hit counts
    /// and cache-creation counts accumulate across turns; silently dropping
    /// either would undercount multi-turn cache activity in reporting.
    #[test]
    fn merge_usage_sums_prompt_tokens_details_across_turns() {
        use genai::chat::{PromptTokensDetails, Usage};
        let a = Usage {
            prompt_tokens: Some(200),
            completion_tokens: Some(30),
            total_tokens: Some(230),
            prompt_tokens_details: Some(PromptTokensDetails {
                cache_creation_tokens: Some(50),
                cache_creation_details: None,
                cached_tokens: Some(100),
                audio_tokens: None,
            }),
            completion_tokens_details: None,
        };
        let b = Usage {
            prompt_tokens: Some(180),
            completion_tokens: Some(20),
            total_tokens: Some(200),
            prompt_tokens_details: Some(PromptTokensDetails {
                cache_creation_tokens: Some(10),
                cache_creation_details: None,
                cached_tokens: Some(150),
                audio_tokens: None,
            }),
            completion_tokens_details: None,
        };
        let merged = merge_usage(a, b);
        let details = merged
            .prompt_tokens_details
            .expect("details should be Some after merging two Some values");
        // cache_creation_tokens: 50 + 10 = 60.
        assert_eq!(details.cache_creation_tokens, Some(60));
        // cached_tokens: 100 + 150 = 250.
        assert_eq!(details.cached_tokens, Some(250));
        // audio_tokens: None + None = None.
        assert_eq!(details.audio_tokens, None);
    }

    /// When only one side has `prompt_tokens_details`, the result preserves
    /// the non-None side (identity law for None).
    #[test]
    fn merge_usage_details_identity_when_one_side_is_none() {
        use genai::chat::{PromptTokensDetails, Usage};
        let with_details = Usage {
            prompt_tokens: Some(100),
            completion_tokens: Some(10),
            total_tokens: Some(110),
            prompt_tokens_details: Some(PromptTokensDetails {
                cache_creation_tokens: None,
                cache_creation_details: None,
                cached_tokens: Some(80),
                audio_tokens: None,
            }),
            completion_tokens_details: None,
        };
        let without_details = Usage {
            prompt_tokens: Some(50),
            completion_tokens: Some(5),
            total_tokens: Some(55),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        };
        // a has details, b does not.
        let merged_a_b = merge_usage(with_details.clone(), without_details.clone());
        assert_eq!(
            merged_a_b
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(80)
        );
        // b has details, a does not.
        let merged_b_a = merge_usage(without_details, with_details);
        assert_eq!(
            merged_b_a
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(80)
        );
    }

    // ---- Integration tests: orchestrate + drive_step via MockProviderClient ----

    use crate::testing::{InMemoryMemoryStore, MockProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
    use pattern_core::types::ids::{BatchId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaSnapshot;

    /// Build a SessionContext wired to a MockProviderClient returning
    /// the given scripted turns. Returns `(ctx, vec_sink, provider)`.
    /// The provider is returned separately so tests can assert
    /// `call_count` post-run.
    async fn mock_session(
        turns: Vec<Vec<genai::chat::ChatStreamEvent>>,
    ) -> (Arc<SessionContext>, Arc<VecSink>, Arc<MockProviderClient>) {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider_concrete = Arc::new(MockProviderClient::with_turns(turns));
        let provider: Arc<dyn ProviderClient> = provider_concrete.clone();
        let db = crate::testing::test_db().await;
        // Create the agent row so the FK on messages.agent_id is satisfied
        // when drive_step persists messages.
        create_test_agent_row(&db, "agent-a").await;
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();
        let persona = PersonaSnapshot::new("agent-a", "A");
        let ctx = Arc::new(
            SessionContext::from_persona(&persona, store, provider, db).with_turn_sink(sink_dyn),
        );
        (ctx, sink, provider_concrete)
    }

    /// Insert a minimal agent row to satisfy the FK constraint on
    /// `messages.agent_id`.
    async fn create_test_agent_row(db: &pattern_db::ConstellationDb, id: &str) {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: "Test".to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(db.pool(), &agent)
            .await
            .expect("create_test_agent_row");
    }

    fn test_turn_input() -> TurnInput {
        // Fresh batch start: turn_id == batch_id (first turn IS the batch).
        let id = new_snowflake_id();
        TurnInput {
            turn_id: id.clone(),
            batch_id: BatchId::from(id),
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
            mock_session(vec![MockProviderClient::text_turn("Hello, world!")]).await;

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
        assert!(out.tool_results().is_empty());
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
        )])
        .await;

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
        )])
        .await;

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
        // tool_results are now inlined into messages; access via the method.
        let results = out.tool_results();
        assert_eq!(results.len(), 1, "tool_result message should be inlined");
        assert_eq!(results[0].call_id, "toolu_01");
        assert!(
            matches!(results[0].outcome, ToolOutcome::Success(_)),
            "dispatcher should have succeeded"
        );
        // On a tool-use turn, messages should be [assistant, tool_result].
        assert_eq!(
            out.messages.len(),
            2,
            "tool-use TurnOutput must carry both assistant and tool_result messages"
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
        ])
        .await;

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

        // Turn 1 (tool_use): messages should be [assistant(tool_use), tool_result].
        assert_eq!(
            reply.turns[0].messages.len(),
            2,
            "tool-use turn must carry both assistant and tool_result messages"
        );
        assert_eq!(
            reply.turns[0].messages[1].chat_message.role,
            genai::chat::ChatRole::Tool,
            "second message of tool-use turn must be the tool_result"
        );

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
        )])
        .await;

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

        let (ctx, _sink, _) = mock_session(vec![no_usage_turn]).await;
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
        )])
        .await;
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
        )])
        .await;

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
        )])
        .await;

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
        ])
        .await;

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
        // The error outcome rode in the tool_result message content. After the
        // TurnHistory refactor, tool_results are inlined into TurnOutput.messages
        // as a ChatRole::Tool message; the accessor returns the content (which
        // is the error string encoded as a JSON Value::String by `to_tool_response`).
        // We verify the round-trip by checking the turn had a tool_result message.
        let turn0 = &reply.turns[0];
        assert_eq!(turn0.stop_reason, StopReason::ToolUse);
        assert_eq!(
            turn0.messages.len(),
            2,
            "tool-use turn must carry [assistant, tool_result] messages"
        );
        assert_eq!(
            turn0.messages[1].chat_message.role,
            genai::chat::ChatRole::Tool,
            "second message must be the tool_result"
        );
        assert_eq!(reply.final_stop_reason, StopReason::EndTurn);
    }

    // ---- Multi-turn tool-use history correctness tests ----------------------
    //
    // These tests verify the core fix: that TurnHistory preserves the full
    // conversational round-trip (user input + assistant reply + tool_result)
    // so the composer's Segment 2 pass replays a valid message sequence.

    /// Drive two tool-use turns, then assert that TurnHistory contains the
    /// correct message sequence for composing a third wire turn.
    ///
    /// Expected sequence after two tool-use turns in history:
    ///   turn 0: input=[user_msg], output=[assistant_1(tool_use), tool_result_1]
    ///   turn 1: input=[],         output=[assistant_2(tool_use), tool_result_2]
    ///
    /// `active_messages()` should yield:
    ///   [user_msg, assistant_1, tool_result_1, assistant_2, tool_result_2]
    ///
    /// That is the valid Anthropic wire sequence for a third request.
    #[tokio::test]
    async fn drive_step_two_tool_use_turns_history_yields_correct_message_order() {
        use genai::chat::ChatRole;

        // Three-turn script: two tool_use turns then a terminal text turn.
        let (ctx, _sink, provider) = mock_session(vec![
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "pure ()"}),
            ),
            MockProviderClient::tool_use_turn(
                "toulu_02",
                "code",
                serde_json::json!({"code": "pure ()"}),
            ),
            MockProviderClient::text_turn("All done."),
        ])
        .await;

        // Fresh batch: batch_id = turn_id = first message's batch, all the same snowflake.
        let batch_snowflake = new_snowflake_id();
        let user_msg = {
            use pattern_core::types::ids::{AgentId, MessageId};
            Message {
                chat_message: genai::chat::ChatMessage::user("check on me"),
                id: MessageId::from(new_id()),
                position: new_snowflake_id(),
                owner_id: AgentId::from("agent-a"),
                created_at: jiff::Timestamp::now(),
                batch: batch_snowflake.clone(),
                response_meta: None,
                block_refs: vec![],
                attachments: vec![],
            }
        };

        let turn_history = Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty()));
        let initial_input = {
            use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
            TurnInput {
                turn_id: batch_snowflake.clone(),
                batch_id: BatchId::from(batch_snowflake.clone()),
                origin: MessageOrigin::new(
                    Author::System {
                        reason: SystemReason::Wakeup,
                    },
                    Sphere::System,
                ),
                messages: vec![user_msg],
            }
        };

        let dispatcher = MockSuccessDispatcher::default();
        let reply = drive_step(
            initial_input,
            ctx,
            turn_history.clone(),
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed");

        assert_eq!(provider.call_count(), 3, "three wire turns expected");
        assert_eq!(reply.turns.len(), 3);

        // --- Assert TurnHistory has 3 records with correct structure ---

        let hist = turn_history.lock().unwrap();
        assert_eq!(hist.active_len(), 3, "three TurnRecords in history");

        let records: Vec<_> = hist.iter_active().collect();

        // Turn 0: input has user message, output has [assistant, tool_result].
        assert_eq!(
            records[0].input.messages.len(),
            1,
            "turn 0 input must carry the original user message"
        );
        assert_eq!(
            records[0].input.messages[0].chat_message.role,
            ChatRole::User,
            "turn 0 input message must be user-role"
        );
        assert_eq!(
            records[0].output.messages.len(),
            2,
            "turn 0 output must have [assistant, tool_result]"
        );
        assert_eq!(
            records[0].output.messages[0].chat_message.role,
            ChatRole::Assistant,
            "turn 0 output[0] must be assistant"
        );
        assert_eq!(
            records[0].output.messages[1].chat_message.role,
            ChatRole::Tool,
            "turn 0 output[1] must be tool_result"
        );

        // Turn 1: continuation — input is empty, output has [assistant, tool_result].
        assert_eq!(
            records[1].input.messages.len(),
            0,
            "turn 1 input must be empty (continuation)"
        );
        assert_eq!(
            records[1].output.messages.len(),
            2,
            "turn 1 output must have [assistant, tool_result]"
        );

        // Turn 2: continuation — input is empty, output has just [assistant].
        assert_eq!(
            records[2].input.messages.len(),
            0,
            "turn 2 input must be empty (continuation)"
        );
        assert_eq!(
            records[2].output.messages.len(),
            1,
            "turn 2 output must have just [assistant] (EndTurn, no tool_result)"
        );
        assert_eq!(
            records[2].output.messages[0].chat_message.role,
            ChatRole::Assistant,
        );

        // --- Assert active_messages() yields correct order for composing turn 3 ---

        let msg_roles: Vec<ChatRole> = hist
            .active_messages()
            .map(|m| m.chat_message.role.clone())
            .collect();

        // Expected: [User, Assistant, Tool, Assistant, Tool, Assistant]
        // = [user_msg] + [asst_1, tr_1] + [] + [asst_2, tr_2] + [] + [asst_3]
        assert_eq!(
            msg_roles,
            vec![
                ChatRole::User,
                ChatRole::Assistant,
                ChatRole::Tool,
                ChatRole::Assistant,
                ChatRole::Tool,
                ChatRole::Assistant,
            ],
            "active_messages must yield the correct Anthropic wire order: \
             user, asst_1(tool_use), tool_result_1, asst_2(tool_use), tool_result_2, asst_3"
        );
    }

    // ---- Splice mutation safety test ----------------------------------------
    //
    // Verifies that the splice in compose_request_for_turn CLONES from history
    // (via the prior_messages snapshot taken before compose) and does NOT
    // mutate the original TurnRecord in TurnHistory. After the splice runs,
    // the tool_result in history must have its original content (no seg3 baked in).

    #[tokio::test]
    async fn splice_does_not_mutate_turn_history_tool_result_content() {
        use genai::chat::{ChatRole, ContentPart};

        // Two turns: tool_use then final text. The splice happens on the
        // second wire turn's compose_request_for_turn call.
        let (ctx, _sink, _provider) = mock_session(vec![
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "pure ()"}),
            ),
            MockProviderClient::text_turn("Done."),
        ])
        .await;

        let turn_history = Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty()));
        let dispatcher = MockSuccessDispatcher::default();

        drive_step(
            test_turn_input(),
            ctx,
            turn_history.clone(),
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed");

        // After drive_step, history must have 2 TurnRecords.
        let hist = turn_history.lock().unwrap();
        assert_eq!(hist.active_len(), 2);

        let records: Vec<_> = hist.iter_active().collect();
        let turn0_tool_msg = &records[0].output.messages[1];
        assert_eq!(
            turn0_tool_msg.chat_message.role,
            ChatRole::Tool,
            "second output message of turn 0 must be the tool_result"
        );

        // Verify the tool_result message in history has NOT been modified by the
        // seg3 splice. The splice operates on the composed req (which clones
        // from prior_messages), NOT on the stored TurnRecord. The stored content
        // must be the plain tool output, not an array with a prepended seg3 block.
        for part in turn0_tool_msg.chat_message.content.parts() {
            if let ContentPart::ToolResponse(tr) = part {
                assert!(
                    !tr.content.is_array() || {
                        // If it IS an array, it must NOT have a seg3 text block as first element.
                        // The seg3 block has the key "type" = "text" and text starting with
                        // "[memory:current_state]".
                        let arr = tr.content.as_array().unwrap();
                        !arr.first()
                            .and_then(|v| v.get("text"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.contains("current_state"))
                            .unwrap_or(false)
                    },
                    "splice must NOT have mutated the stored tool_result content in TurnHistory; \
                     found seg3 content baked into the stored ToolResponse: {:?}",
                    tr.content
                );
            }
        }
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
        let mut msg = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(vec![ContentPart::ToolResponse(tool_response)]),
            options: None,
        };

        // Use the production splice function.
        splice_text_onto_message(&mut msg, "seg3 memory context");

        assert_eq!(
            msg.role,
            ChatRole::Tool,
            "role MUST remain Tool after splice — flipping to User causes Anthropic 400"
        );

        // Verify the content was actually folded.
        let parts = msg.content.parts();
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

    // ---- Attachment + snapshot tests ----------------------------------------

    /// Test helper: build a visible RenderedBlock with Working type.
    fn test_block(label: &str, rendered: &str, hash: u64) -> RenderedBlock {
        RenderedBlock {
            label: smol_str::SmolStr::new(label),
            block_type: pattern_core::memory::BlockType::Working,
            rendered: Some(std::sync::Arc::from(rendered)),
            content_hash: hash,
        }
    }

    #[test]
    fn render_snapshot_attachment_full_contains_block_content() {
        let blocks = vec![test_block(
            "notes",
            "<block:notes type=\"working\" permission=\"read_write\">\nhello\n</block:notes>",
            12345,
        )];
        let attachment = MessageAttachment::BatchOpeningSnapshot {
            kind: SnapshotKind::Full,
            block_names: vec!["notes".into()],
            blocks,
            edited_blocks: vec![],
        };
        let rendered = render_snapshot_attachment(&attachment);
        assert!(
            rendered.contains("<system-reminder>"),
            "must wrap in system-reminder"
        );
        assert!(
            rendered.contains("[memory:current_state]"),
            "must contain tag"
        );
        assert!(rendered.contains("(full snapshot)"), "must indicate full");
        assert!(
            rendered.contains("<block:notes"),
            "must contain block content"
        );
    }

    #[test]
    fn render_snapshot_attachment_delta_shows_edited_blocks() {
        let blocks = vec![test_block(
            "tasks",
            "<block:tasks>changed content</block:tasks>",
            99999,
        )];
        let attachment = MessageAttachment::BatchOpeningSnapshot {
            kind: SnapshotKind::Delta {
                since_batch: "batch-prev".into(),
            },
            block_names: vec!["notes".into(), "tasks".into()],
            blocks,
            edited_blocks: vec!["tasks".into()],
        };
        let rendered = render_snapshot_attachment(&attachment);
        assert!(
            rendered.contains("(delta since batch batch-prev)"),
            "must indicate delta"
        );
        assert!(
            rendered.contains("[memory:updated]"),
            "must have updated marker"
        );
        assert!(rendered.contains("tasks"), "must mention edited block");
        assert!(
            rendered.contains("Available blocks: notes, tasks"),
            "must list all blocks"
        );
    }

    #[test]
    fn build_snapshot_full_includes_all_blocks() {
        let blocks = vec![
            test_block("a", "content-a", 1),
            test_block("b", "content-b", 2),
        ];
        let att = build_snapshot_attachment(SnapshotKind::Full, blocks, None);
        let MessageAttachment::BatchOpeningSnapshot {
            kind,
            block_names,
            blocks,
            edited_blocks,
        } = &att;
        assert_eq!(*kind, SnapshotKind::Full);
        assert_eq!(block_names.len(), 2);
        assert_eq!(blocks.len(), 2);
        assert!(edited_blocks.is_empty());
    }

    #[test]
    fn build_snapshot_delta_only_includes_changed_blocks() {
        // Prior: {"a" => hash 1, "b" => hash 2}
        let mut prior_hashes = std::collections::HashMap::new();
        prior_hashes.insert("a".to_string(), 1u64);
        prior_hashes.insert("b".to_string(), 2u64);

        // Current: "a" unchanged (hash=1), "b" changed (hash=99), "c" new.
        let current = vec![
            test_block("a", "content-a", 1),
            test_block("b", "content-b-v2", 99),
            test_block("c", "content-c", 3),
        ];

        let att = build_snapshot_attachment(
            SnapshotKind::Delta {
                since_batch: "prev".into(),
            },
            current,
            Some(prior_hashes),
        );
        let MessageAttachment::BatchOpeningSnapshot {
            block_names,
            blocks,
            edited_blocks,
            ..
        } = &att;

        // block_names always has ALL current blocks.
        assert_eq!(block_names.len(), 3);
        // Only "b" (changed) and "c" (new) should be in the delta.
        assert_eq!(edited_blocks.len(), 2);
        assert!(edited_blocks.contains(&smol_str::SmolStr::new("b")));
        assert!(edited_blocks.contains(&smol_str::SmolStr::new("c")));
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn content_hash_stable_for_same_input() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        assert_eq!(h1, h2, "same input must produce same hash");
    }

    #[test]
    fn content_hash_differs_for_different_input() {
        let h1 = content_hash("hello");
        let h2 = content_hash("world");
        assert_ne!(h1, h2, "different inputs should produce different hashes");
    }

    // ---- Cache stability: attachment on batch-opening message stays stable ---

    // ---- MidBatchDeltaBehavior policy tests ---------------------------------

    /// `MidBatchDeltaBehavior::default()` must be `IncludeSelfEdits`. This is
    /// the conservative default: the agent receives post-edit block state so
    /// it can verify its writes landed, at the cost of cache churn on every
    /// memory-editing turn. The default preserves current behavior while
    /// making the policy axis explicit.
    #[test]
    fn mid_batch_delta_behavior_default_is_include_self_edits() {
        use pattern_core::types::message::MidBatchDeltaBehavior;
        assert_eq!(
            MidBatchDeltaBehavior::default(),
            MidBatchDeltaBehavior::IncludeSelfEdits,
        );
    }

    /// `SnapshotPolicy::default()` must have `MidBatchDeltaBehavior::IncludeSelfEdits`
    /// and the standard Core+Working selection. Ensures the scaffolding
    /// composes correctly and the default is observable end-to-end.
    #[test]
    fn snapshot_policy_default_has_include_self_edits_and_standard_selection() {
        use pattern_core::memory::BlockType;
        use pattern_core::types::message::{MidBatchDeltaBehavior, SnapshotPolicy};
        let policy = SnapshotPolicy::default();
        assert_eq!(policy.mid_batch, MidBatchDeltaBehavior::IncludeSelfEdits);
        assert!(
            policy.selection.include_types.contains(&BlockType::Core),
            "default selection must include Core blocks"
        );
        assert!(
            policy.selection.include_types.contains(&BlockType::Working),
            "default selection must include Working blocks"
        );
        assert!(
            policy.selection.include_labels.is_empty(),
            "default selection has no explicit label allowlist"
        );
        assert!(
            policy.selection.exclude_labels.is_empty(),
            "default selection has no explicit label exclusions"
        );
    }

    /// `MidBatchDeltaBehavior::IncludeSelfEdits` — when the in-memory store
    /// has a block that wasn't in prior history (treated as "new"), and
    /// the tool_use turn does NOT write to it (block_writes is empty), the
    /// delta fires. This verifies the IncludeSelfEdits path's baseline:
    /// purely external changes always emit a delta regardless of policy.
    ///
    /// Implementation note: a full integration test that verifies
    /// FilterSelfEdits suppresses a delta for a block that IS in
    /// block_writes requires driving tool execution through the real
    /// MemoryHandler path (MemoryHandler → adapter.record_write). That
    /// path is exercised by the session-level integration tests; here we
    /// verify the type-level policy contract.
    #[test]
    fn mid_batch_delta_include_self_edits_emits_for_own_writes() {
        use pattern_core::types::message::{MidBatchDeltaBehavior, SnapshotPolicy};
        // Under IncludeSelfEdits, the self_written set is always empty —
        // every changed block triggers a delta regardless of who wrote it.
        // Verify that a non-empty block_writes list does NOT suppress the
        // self_written set (it stays empty, so all deltas are allowed).
        let policy = SnapshotPolicy {
            selection: Default::default(),
            mid_batch: MidBatchDeltaBehavior::IncludeSelfEdits,
        };
        // Simulate: is this label in the self_written set under IncludeSelfEdits?
        // Under IncludeSelfEdits the set is always empty, so no label is filtered.
        let simulated_self_written: std::collections::HashSet<&str> =
            if matches!(policy.mid_batch, MidBatchDeltaBehavior::FilterSelfEdits) {
                ["notes"].iter().copied().collect()
            } else {
                std::collections::HashSet::new()
            };
        assert!(
            !simulated_self_written.contains("notes"),
            "under IncludeSelfEdits, no label is in self_written — all deltas fire"
        );
    }

    /// `MidBatchDeltaBehavior::FilterSelfEdits` — when `block_writes` contains
    /// a label, that label is excluded from mid-batch delta consideration.
    /// Only truly external changes (labels NOT in block_writes) still emit.
    #[test]
    fn mid_batch_delta_filter_self_edits_skips_own_writes() {
        use pattern_core::types::message::{MidBatchDeltaBehavior, SnapshotPolicy};
        let policy = SnapshotPolicy {
            selection: Default::default(),
            mid_batch: MidBatchDeltaBehavior::FilterSelfEdits,
        };
        // Simulate block_writes containing "notes" — agent wrote it this turn.
        let self_written_labels = ["notes"];

        // Reproduce the filtering logic from drive_step.
        let self_written: std::collections::HashSet<&str> =
            if matches!(policy.mid_batch, MidBatchDeltaBehavior::FilterSelfEdits) {
                self_written_labels.iter().copied().collect()
            } else {
                std::collections::HashSet::new()
            };

        // Under FilterSelfEdits, "notes" (self-written) is excluded.
        assert!(
            self_written.contains("notes"),
            "under FilterSelfEdits, self-written labels are in the exclusion set"
        );
        // An external change on "tasks" (not in block_writes) is NOT excluded.
        assert!(
            !self_written.contains("tasks"),
            "under FilterSelfEdits, labels not in block_writes still emit deltas"
        );

        // Verify the has_external_changes logic: a block with the self-written
        // label is suppressed, but a block with a different label fires.
        let prior_hashes: std::collections::HashMap<String, u64> = {
            let mut m = std::collections::HashMap::new();
            m.insert("notes".to_string(), 111_u64);
            m.insert("tasks".to_string(), 222_u64);
            m
        };
        let current_blocks = [
            // "notes" changed hash — but it's self-written, should be filtered.
            test_block("notes", "new notes content", 999),
            // "tasks" changed hash — external change, should NOT be filtered.
            test_block("tasks", "new tasks content", 888),
        ];
        let has_external_changes = current_blocks.iter().any(|b| {
            let label = b.label.as_str();
            if self_written.contains(label) {
                return false;
            }
            prior_hashes
                .get(label)
                .map(|&h| h != b.content_hash)
                .unwrap_or(true)
        });
        assert!(
            has_external_changes,
            "external change on 'tasks' must still trigger delta even under FilterSelfEdits"
        );

        // When ALL changed blocks are self-written, no delta fires.
        let only_self_written_changes = [
            test_block("notes", "new notes content", 999), // self-written, filtered
        ];
        let has_only_self_changes = only_self_written_changes.iter().any(|b| {
            let label = b.label.as_str();
            if self_written.contains(label) {
                return false;
            }
            prior_hashes
                .get(label)
                .map(|&h| h != b.content_hash)
                .unwrap_or(true)
        });
        assert!(
            !has_only_self_changes,
            "when ALL changes are self-written, FilterSelfEdits must suppress the delta"
        );
    }

    // ---- FilterSelfEdits / IncludeSelfEdits integration tests ---------------
    //
    // These tests drive `drive_step` end-to-end with a real `SessionContext`
    // and a `MockProviderClient` scripted to produce a tool_use turn followed
    // by a final text turn. A `WriteRecordingDispatcher` simulates what
    // `MemoryHandler` does in production: during tool dispatch it records a
    // `BlockWrite` on `ctx.adapter()` so `turn.block_writes` is non-empty.
    //
    // Combined with a Working block pre-created in the memory store, this
    // exercises the full mid-batch delta path and verifies that:
    // - `FilterSelfEdits` suppresses a delta for a block the agent wrote this
    //   turn (no `BatchOpeningSnapshot` on the tool_result message).
    // - `IncludeSelfEdits` includes that same block in the delta (a
    //   `BatchOpeningSnapshot` IS present on the tool_result message).

    /// A dispatcher that, on each dispatch, records a `BlockWrite` for the
    /// given label on the session's memory adapter. This simulates what
    /// `MemoryHandler` does during real tool execution without going through
    /// the Haskell eval path.
    struct WriteRecordingDispatcher {
        ctx: Arc<SessionContext>,
        block_label: String,
    }

    #[async_trait]
    impl EvalDispatcher for WriteRecordingDispatcher {
        async fn dispatch(&self, _tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
            use jiff::Timestamp;
            use pattern_core::memory::BlockType;
            use pattern_core::types::block::{BlockWrite, BlockWriteKind};

            self.ctx.adapter().record_write(BlockWrite {
                handle: smol_str::SmolStr::new(&self.block_label),
                memory_id: smol_str::SmolStr::new("mem-test"),
                block_type: BlockType::Working,
                rendered_content: "updated content".to_string(),
                kind: BlockWriteKind::Replaced,
                previous_content_hash: Some(0xabcd),
                previous_rendered_content: Some("original content".to_string()),
                at: Timestamp::now(),
                author: pattern_core::types::origin::Author::System {
                    reason: pattern_core::types::origin::SystemReason::ToolCall,
                },
            });
            ToolOutcome::Success(serde_json::json!({"ok": true}))
        }
    }

    /// Build a session with a custom `SnapshotPolicy` applied via
    /// `ContextPolicy::snapshot_policy`. Also pre-creates a Working block
    /// in the memory store so it appears in the mid-batch delta scan.
    async fn mock_session_with_policy(
        turns: Vec<Vec<genai::chat::ChatStreamEvent>>,
        mid_batch: pattern_core::types::message::MidBatchDeltaBehavior,
        block_label: &str,
    ) -> (Arc<SessionContext>, Arc<VecSink>, Arc<MockProviderClient>) {
        use pattern_core::memory::BlockType;
        use pattern_core::types::block::BlockCreate;
        use pattern_core::types::message::SnapshotPolicy;
        use pattern_core::types::snapshot::ContextPolicy;

        let store_concrete = Arc::new(InMemoryMemoryStore::new());
        // Pre-create the Working block so it is visible to the snapshot scan.
        store_concrete
            .create_block(
                "agent-a",
                BlockCreate::new(
                    block_label,
                    BlockType::Working,
                    pattern_core::memory::BlockSchema::text(),
                ),
            )
            .await
            .expect("pre-create block");

        let store: Arc<dyn MemoryStore> = store_concrete;
        let provider_concrete = Arc::new(MockProviderClient::with_turns(turns));
        let provider: Arc<dyn pattern_core::ProviderClient> = provider_concrete.clone();
        let db = crate::testing::test_db().await;
        create_test_agent_row(&db, "agent-a").await;
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let persona = PersonaSnapshot::new("agent-a", "A").with_context_policy({
            let mut cp = ContextPolicy::default();
            cp.snapshot_policy = SnapshotPolicy {
                selection: Default::default(),
                mid_batch,
            };
            cp
        });
        let ctx = Arc::new(
            crate::session::SessionContext::from_persona(&persona, store, provider, db)
                .with_turn_sink(sink_dyn),
        );
        (ctx, sink, provider_concrete)
    }

    #[tokio::test]
    async fn drive_step_filter_self_edits_suppresses_delta_for_own_block_write() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let block_label = "notes";
        let (ctx, _sink, _provider) = mock_session_with_policy(
            vec![
                MockProviderClient::tool_use_turn(
                    "toolu_01",
                    "code",
                    serde_json::json!({"code": "Memory.put \"notes\" \"updated content\""}),
                ),
                MockProviderClient::text_turn("I updated your notes."),
            ],
            MidBatchDeltaBehavior::FilterSelfEdits,
            block_label,
        )
        .await;

        let dispatcher = WriteRecordingDispatcher {
            ctx: ctx.clone(),
            block_label: block_label.to_string(),
        };

        let hist = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));
        let reply = drive_step(
            test_turn_input(),
            ctx,
            hist,
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed");

        // The tool_use turn is turns[0]; its messages are [assistant, tool_result].
        let tool_result_msg = reply.turns[0]
            .messages
            .iter()
            .find(|m| m.chat_message.role == genai::chat::ChatRole::Tool)
            .expect("should have a tool_result message");

        // Under FilterSelfEdits the agent's own write to 'notes' is in
        // self_written; has_external_changes must be false and no
        // BatchOpeningSnapshot should be attached to the tool_result.
        let snapshot_attachments: Vec<_> = tool_result_msg
            .attachments
            .iter()
            .filter(|a| matches!(a, MessageAttachment::BatchOpeningSnapshot { .. }))
            .collect();
        assert!(
            snapshot_attachments.is_empty(),
            "FilterSelfEdits: tool_result message must NOT have a BatchOpeningSnapshot \
             for a block the agent itself wrote this turn (got {} attachments)",
            snapshot_attachments.len()
        );
    }

    #[tokio::test]
    async fn drive_step_include_self_edits_emits_delta_for_own_block_write() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let block_label = "notes";
        let (ctx, _sink, _provider) = mock_session_with_policy(
            vec![
                MockProviderClient::tool_use_turn(
                    "toolu_01",
                    "code",
                    serde_json::json!({"code": "Memory.put \"notes\" \"updated content\""}),
                ),
                MockProviderClient::text_turn("I updated your notes."),
            ],
            MidBatchDeltaBehavior::IncludeSelfEdits,
            block_label,
        )
        .await;

        let dispatcher = WriteRecordingDispatcher {
            ctx: ctx.clone(),
            block_label: block_label.to_string(),
        };

        let hist = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));
        let reply = drive_step(
            test_turn_input(),
            ctx,
            hist,
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
            &dispatcher,
            "",
        )
        .await
        .expect("drive_step should succeed");

        let tool_result_msg = reply.turns[0]
            .messages
            .iter()
            .find(|m| m.chat_message.role == genai::chat::ChatRole::Tool)
            .expect("should have a tool_result message");

        // Under IncludeSelfEdits the self_written set is always empty, so
        // the 'notes' block (which is new in the store and thus has no prior
        // hash) triggers has_external_changes = true and a BatchOpeningSnapshot
        // is attached to the tool_result message.
        let snapshot_attachments: Vec<_> = tool_result_msg
            .attachments
            .iter()
            .filter(|a| matches!(a, MessageAttachment::BatchOpeningSnapshot { .. }))
            .collect();
        assert_eq!(
            snapshot_attachments.len(),
            1,
            "IncludeSelfEdits: tool_result message MUST have a BatchOpeningSnapshot \
             even for blocks the agent wrote (self-edits are visible for agent verification)"
        );
    }

    #[test]
    fn splice_text_onto_user_message_appends_text_part() {
        let mut msg = ChatMessage::user("original");
        splice_text_onto_message(&mut msg, "appended");
        let parts = msg.content.parts();
        assert_eq!(parts.len(), 2, "original text + appended text");
        assert_eq!(msg.role, genai::chat::ChatRole::User);
    }

    #[test]
    fn splice_text_onto_tool_message_folds_into_tool_response() {
        use genai::chat::{ChatRole, ContentPart, MessageContent, ToolResponse};

        let tr = ToolResponse::new("call_01", "result");
        let mut msg = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(vec![ContentPart::ToolResponse(tr)]),
            options: None,
        };
        splice_text_onto_message(&mut msg, "memory snapshot");

        assert_eq!(msg.role, ChatRole::Tool);
        let ContentPart::ToolResponse(ref tr) = msg.content.parts()[0] else {
            panic!("expected ToolResponse");
        };
        let arr = tr.content.as_array().expect("must be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "memory snapshot");
        assert_eq!(arr[1]["text"], "result");
    }
}
