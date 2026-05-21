//! Composer pipeline — [`ComposerPass`] trait, orchestrator, and
//! finalization scaffolding.
//!
//! # Pipeline semantics
//!
//! 1. Caller constructs an empty
//!    [`super::partial_request::PartialRequest`] targeting a model.
//! 2. Caller configures a `Vec<Box<dyn ComposerPass>>` in the desired
//!    order. Phase 5 default: Segment1 → Segment2 → Segment3 (added by
//!    Phase 5 Tasks 8-9).
//! 3. [`compose`] applies each pass in order, wrapping any error in
//!    [`ProviderError::ComposerPassFailed`] so the failing pass's name
//!    survives the error bubble-up.
//! 4. [`finalize`] assembles the accumulated partial into a finished
//!    [`pattern_core::types::provider::CompletionRequest`] consumed by
//!    [`pattern_core::traits::provider_client::ProviderClient::complete`].
//!
//! # I/O policy
//!
//! Composer passes MUST NOT perform I/O. All data a pass needs must be
//! captured at construction time. This keeps passes synchronous (the
//! trait is `fn apply`, not `async fn`), deterministic (safe for
//! break-detection replay), and unit-testable (pure functions over
//! pure input).
//!
//! I/O-producing data (rendered memory blocks, tool schemas, etc.)
//! gets pre-computed by the turn loop and fed into pass constructors,
//! not looked up from inside `apply`.
//!
//! # Finalize
//!
//! [`finalize`] converts the accumulated partial into a finished
//! [`pattern_core::types::provider::CompletionRequest`]:
//!
//! 1. **Budget recheck** — belt-and-suspenders count validation on
//!    top of the tracker's placement-time enforcement.
//! 2. **Marker application** — attaches each placement's
//!    `CacheControl` to the indexed `SystemBlock.cache_control` or
//!    `ChatMessage.options.cache_control`.
//! 3. **Extended-TTL beta header check** — verifies the
//!    `extended-cache-ttl-2025-04-11` beta header is present when any
//!    placement uses `Ephemeral1h` or `Ephemeral24h`.
//! 4. **TTL ordering** — walks the wire-format sequence (system blocks
//!    → messages) and rejects short-TTL-before-long-TTL patterns.

use genai::chat::{
    CacheControl, ChatMessage, ContentPart, MessageContent, MessageOptions, SystemBlock,
};
use pattern_core::error::ProviderError;
use pattern_core::types::provider::CompletionRequest;
use smol_str::SmolStr;

use super::breakpoints::{BreakpointLocation, BreakpointTracker};
use super::partial_request::PartialRequest;

/// Finalized compose output: the wire request plus origin tags for
/// each composed message. The runtime uses `message_origins` to look
/// up composed messages by Pattern `MessageId` instead of fragile
/// index arithmetic.
#[derive(Debug)]
pub struct ComposeOutput {
    /// The finalized completion request ready for the provider.
    pub request: CompletionRequest,
    /// Origin tags parallel to `request.chat.messages`. Each entry is
    /// `Some(message_id)` for messages that originated from a Pattern
    /// `Message`, or `None` for synthetic messages.
    pub message_origins: Vec<Option<SmolStr>>,
}

/// A single transformation step in the composer pipeline. See
/// [module docs][self] for the I/O policy.
pub trait ComposerPass: Send + Sync {
    /// Static identifier, used in error messages and break-detection
    /// logs. Must be a string literal (e.g. `"segment_1"`); production
    /// passes should not include dynamic content in the name.
    fn name(&self) -> &'static str;

    /// Apply this pass to the partial request. Passes may mutate
    /// headers, system blocks, messages, tools, and the breakpoint
    /// tracker. Passes MUST NOT perform I/O.
    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError>;
}

/// Run `passes` in order against `initial`, then finalize the result.
/// Pass errors are wrapped in [`ProviderError::ComposerPassFailed`] so
/// the failing pass's name survives the bubble-up.
///
/// Returns a [`ComposeOutput`] containing the finalized request plus
/// message-origin tags that map each composed message back to its
/// Pattern `MessageId` (or `None` for synthetic messages).
pub fn compose(
    passes: &[Box<dyn ComposerPass>],
    initial: PartialRequest,
) -> Result<ComposeOutput, ProviderError> {
    let mut partial = initial;
    for pass in passes {
        pass.apply(&mut partial)
            .map_err(|source| ProviderError::ComposerPassFailed {
                pass: pass.name().to_string(),
                source: Box::new(source),
            })?;
    }
    // Extract message_origins before finalize consumes the partial.
    let message_origins = partial.message_origins.clone();
    let request = finalize(partial)?;
    Ok(ComposeOutput {
        request,
        message_origins,
    })
}

/// Assemble a completed [`PartialRequest`] into a [`CompletionRequest`].
///
/// Validates breakpoint budget, applies `cache_control` markers to
/// their indexed targets, and validates TTL ordering (Anthropic's
/// wire-format constraint). The extended-TTL beta header check that
/// used to live here was retired in Phase 5 Task 20 — Anthropic
/// dropped the header as a routing requirement in late 2025. See
/// [module docs][self] for the full list.
pub fn finalize(partial: PartialRequest) -> Result<CompletionRequest, ProviderError> {
    let PartialRequest {
        model,
        mut system_blocks,
        mut messages,
        tools,
        options,
        // `extra_headers` is consumed by the gateway shaper at wire
        // serialisation time, not by finalize itself. The
        // `extended-cache-ttl-2025-04-11` check that used to read
        // this field was retired (header no longer required by
        // Anthropic).
        extra_headers: _,
        breakpoints,
        // `message_origins` is consumed by the runtime's splice
        // logic via ComposeOutput — finalize doesn't need it.
        message_origins: _,
    } = partial;

    // 1. Belt-and-suspenders budget recheck (AC7.5). place() already
    //    enforces at placement time, but validate here too.
    if breakpoints.count() > BreakpointTracker::ANTHROPIC_MAX_BREAKPOINTS {
        return Err(ProviderError::CacheBreakpointBudgetExceeded {
            budget: BreakpointTracker::ANTHROPIC_MAX_BREAKPOINTS,
            placed_by: breakpoints
                .placements()
                .iter()
                .map(|p| p.placed_by_pass.to_string())
                .collect(),
            attempted_by: "finalize".to_string(),
        });
    }

    // 2. Apply each placement to the indexed block/message.
    for placement in breakpoints.placements() {
        match placement.location {
            BreakpointLocation::SystemBlock(idx) => {
                let block = system_blocks.get_mut(idx).ok_or_else(|| {
                    ProviderError::InvalidBreakpointLocation {
                        location: "system".into(),
                        idx,
                    }
                })?;
                block.cache_control = Some(placement.control.clone());
            }
            BreakpointLocation::MessageBlock(idx) => {
                let msg = messages.get_mut(idx).ok_or_else(|| {
                    ProviderError::InvalidBreakpointLocation {
                        location: "message".into(),
                        idx,
                    }
                })?;
                // Anthropic rejects cache_control on `thinking` / `redacted_thinking`
                // blocks (400 `Extra inputs are not permitted`). If a message's
                // content is exclusively thinking blocks, the adapter has nothing
                // eligible to attach the marker to and the request will fail.
                // Skip the placement in that case rather than emit an invalid request.
                if message_has_only_thinking_content(msg) {
                    tracing::warn!(
                        idx,
                        placed_by = %placement.placed_by_pass,
                        "skipping cache_control placement: message content is exclusively thinking blocks (anthropic rejects cache_control on thinking)"
                    );
                    continue;
                }
                let opts = msg.options.get_or_insert_with(MessageOptions::default);
                opts.cache_control = Some(placement.control.clone());
            }
            BreakpointLocation::ToolSchema(idx) => {
                // Phase 5 does not place markers on tools. Reject at
                // finalize so future passes get a clear error if the
                // genai Tool type doesn't support cache_control yet.
                return Err(ProviderError::InvalidBreakpointLocation {
                    location: "tool (unsupported in Phase 5)".into(),
                    idx,
                });
            }
        }
    }

    // 3. Extended-TTL beta header check (AC7.5b) — RETIRED.
    //
    // Anthropic dropped the `extended-cache-ttl-2025-04-11` beta as
    // a routing requirement in late 2025. Current endpoints accept
    // `cache_control: { "ttl": "1h" }` directly without the header.
    // The check was defensive redundancy; see the
    // `docs/notes/2026-04-18-cache-ttl-research.md` investigation
    // for the evidence trail. Retired here to unblock agent-loop
    // paths that don't flow through the gateway's shaper (which
    // still emits the marker as a no-op for defence in depth when
    // the beta flag is configured).

    // 4. Strip Binary content parts whose media_type is not accepted by the
    //    provider. Defence-in-depth against earlier seams that may have
    //    routed an unsupported type (e.g. image/svg+xml) into the Binary
    //    path — sending these unmodified produces a wire-level 400 from
    //    Anthropic at both `/v1/messages` and `/v1/messages/count_tokens`.
    strip_unsupported_binary_parts(&mut messages);

    // 5. TTL ordering (Anthropic wire-format constraint).
    validate_ttl_ordering(&system_blocks, &messages, &breakpoints)?;

    // Assemble the final ChatRequest.
    let mut chat = genai::chat::ChatRequest::new(messages);
    if !system_blocks.is_empty() {
        chat.system_blocks = Some(system_blocks);
    }
    if !tools.is_empty() {
        chat.tools = Some(tools);
    }

    Ok(CompletionRequest {
        model,
        chat,
        options,
        persona: None,
    })
}

/// Returns true iff every content part of `msg` is a `ThinkingBlock`.
///
/// Anthropic rejects `cache_control` on `thinking` and `redacted_thinking`
/// blocks. Messages whose content is exclusively thinking have no cache-eligible
/// target for the marker, so placement must be skipped (or the wire request will
/// 400). An empty content vec returns false (no parts → no thinking-only).
fn message_has_only_thinking_content(msg: &genai::chat::ChatMessage) -> bool {
    let parts = msg.content.parts();
    !parts.is_empty() && parts.iter().all(|p| matches!(p, ContentPart::ThinkingBlock(_)))
}

/// Set of binary media types Anthropic accepts on the wire.
///
/// - Vision: `image/jpeg`, `image/png`, `image/gif`, `image/webp`.
/// - Documents: `application/pdf` (PDF feature).
///
/// Anything outside this set — notably `image/svg+xml`, which slips past
/// earlier `image/*` checks but is rejected at the wire — must be stripped
/// before the request hits `/v1/messages` or `/v1/messages/count_tokens`.
fn is_provider_supported_binary_mime(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    matches!(
        ct,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "application/pdf"
    )
}

/// Walk every message, dropping `ContentPart::Binary` parts whose
/// `content_type` is not in the provider's supported set. Recurses into
/// `ContentPart::ToolResponse.content` so binaries nested inside tool
/// results are also filtered.
///
/// Stripped parts are logged at `warn` so the operator can see what went
/// away; the accompanying text marker (when present) survives and gives
/// the agent enough context to know an attachment was elided.
fn strip_unsupported_binary_parts(messages: &mut [ChatMessage]) {
    for (msg_idx, msg) in messages.iter_mut().enumerate() {
        // Pull parts out, filter, put back. MessageContent does not expose a
        // retain-style API directly.
        let old = std::mem::take(&mut msg.content);
        let kept: Vec<ContentPart> = old
            .into_parts()
            .into_iter()
            .filter_map(|part| match part {
                ContentPart::Binary(ref b)
                    if !is_provider_supported_binary_mime(&b.content_type) =>
                {
                    tracing::warn!(
                        msg_idx,
                        content_type = %b.content_type,
                        name = b.name.as_deref().unwrap_or("<unnamed>"),
                        "stripping unsupported binary part from outbound request"
                    );
                    None
                }
                ContentPart::ToolResponse(mut tr) => {
                    tr.content.retain(|p| match p {
                        ContentPart::Binary(b) => {
                            let supported = is_provider_supported_binary_mime(&b.content_type);
                            if !supported {
                                tracing::warn!(
                                    msg_idx,
                                    call_id = %tr.call_id,
                                    content_type = %b.content_type,
                                    name = b.name.as_deref().unwrap_or("<unnamed>"),
                                    "stripping unsupported binary part from tool_result"
                                );
                            }
                            supported
                        }
                        _ => true,
                    });
                    Some(ContentPart::ToolResponse(tr))
                }
                other => Some(other),
            })
            .collect();
        msg.content = MessageContent::from_parts(kept);
    }
}

/// Returns true if the given `CacheControl` is a "short" TTL (5m-class).

fn is_short_ttl(cc: &CacheControl) -> bool {
    matches!(
        cc,
        CacheControl::Ephemeral | CacheControl::Ephemeral5m | CacheControl::Memory
    )
}

/// Returns true if the given `CacheControl` is a "long" TTL (1h/24h-class).
fn is_long_ttl(cc: &CacheControl) -> bool {
    matches!(cc, CacheControl::Ephemeral1h | CacheControl::Ephemeral24h)
}

/// Validate that no short-TTL marker precedes a long-TTL marker in
/// wire-format order (system blocks first, then messages). Anthropic
/// requires 1h/24h entries to appear before 5m entries.
///
/// Uses the breakpoint tracker to find which pass placed each marker,
/// enabling actionable error messages.
fn validate_ttl_ordering(
    system_blocks: &[SystemBlock],
    messages: &[ChatMessage],
    breakpoints: &BreakpointTracker,
) -> Result<(), ProviderError> {
    // Build a flat sequence of (CacheControl, pass_name) in wire order:
    // system blocks first, then messages.
    let mut ordered: Vec<(&CacheControl, &str)> = Vec::new();

    for (idx, block) in system_blocks.iter().enumerate() {
        if let Some(ref cc) = block.cache_control {
            // Find the pass that placed this marker.
            let pass_name = breakpoints
                .placements()
                .iter()
                .find(|p| p.location == BreakpointLocation::SystemBlock(idx))
                .map(|p| p.placed_by_pass)
                .unwrap_or("unknown");
            ordered.push((cc, pass_name));
        }
    }
    for (idx, msg) in messages.iter().enumerate() {
        if let Some(cc) = msg.options.as_ref().and_then(|o| o.cache_control.as_ref()) {
            let pass_name = breakpoints
                .placements()
                .iter()
                .find(|p| p.location == BreakpointLocation::MessageBlock(idx))
                .map(|p| p.placed_by_pass)
                .unwrap_or("unknown");
            ordered.push((cc, pass_name));
        }
    }

    // Walk and check: once we see a short-TTL, any subsequent long-TTL
    // is a violation.
    let mut first_short: Option<&str> = None;
    for (cc, pass_name) in &ordered {
        if is_short_ttl(cc) && first_short.is_none() {
            first_short = Some(pass_name);
        }
        if is_long_ttl(cc)
            && let Some(short_pass) = first_short
        {
            return Err(ProviderError::TtlOrderingViolated {
                short_ttl_pass: short_pass.to_string(),
                long_ttl_pass: pass_name.to_string(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::breakpoints::{BreakpointLocation, BreakpointTracker};
    use super::*;
    use genai::chat::{CacheControl, ChatMessage, SystemBlock};

    /// Pass that appends a tagged system block + places a breakpoint on it.
    struct TagSystemPass(&'static str);
    impl ComposerPass for TagSystemPass {
        fn name(&self) -> &'static str {
            self.0
        }
        fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
            partial
                .system_blocks
                .push(SystemBlock::new(format!("block from {}", self.0)));
            let idx = partial.system_blocks.len() - 1;
            partial.breakpoints.place(
                BreakpointLocation::SystemBlock(idx),
                CacheControl::Ephemeral5m,
                self.0,
            )
        }
    }

    /// Pass that always errors, for the wrap-failure test.
    struct FailingPass;
    impl ComposerPass for FailingPass {
        fn name(&self) -> &'static str {
            "failing_pass"
        }
        fn apply(&self, _partial: &mut PartialRequest) -> Result<(), ProviderError> {
            Err(ProviderError::ShaperMisconfigured {
                reason: "intentional test failure".into(),
            })
        }
    }

    #[test]
    fn compose_runs_passes_in_order_and_accumulates() {
        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(TagSystemPass("alpha")),
            Box::new(TagSystemPass("beta")),
            Box::new(TagSystemPass("gamma")),
        ];
        let output =
            compose(&passes, PartialRequest::new("claude-opus-4-7")).expect("compose succeeds");

        let blocks = output
            .request
            .chat
            .system_blocks
            .as_ref()
            .expect("blocks populated");
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].text.contains("alpha"));
        assert!(blocks[1].text.contains("beta"));
        assert!(blocks[2].text.contains("gamma"));
    }

    #[test]
    fn compose_wraps_pass_errors_with_pass_name() {
        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(TagSystemPass("alpha")),
            Box::new(FailingPass),
            // gamma never runs because FailingPass short-circuits
            Box::new(TagSystemPass("gamma")),
        ];
        let err = compose(&passes, PartialRequest::new("claude-opus-4-7"))
            .expect_err("FailingPass must fail the pipeline");

        match err {
            ProviderError::ComposerPassFailed { pass, source } => {
                assert_eq!(pass, "failing_pass");
                assert!(matches!(*source, ProviderError::ShaperMisconfigured { .. }));
            }
            other => panic!("expected ComposerPassFailed, got {other:?}"),
        }
    }

    // NOTE: compose_budget_exceeded_error_survives_wrap returns Err so
    // the ComposeOutput wrapper doesn't affect it.

    #[test]
    fn compose_budget_exceeded_error_survives_wrap() {
        /// Pass that tries to place two breakpoints but the budget is 1.
        struct TwoMarkerPass;
        impl ComposerPass for TwoMarkerPass {
            fn name(&self) -> &'static str {
                "two_marker_pass"
            }
            fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
                partial.system_blocks.push(SystemBlock::new("block-a"));
                partial.system_blocks.push(SystemBlock::new("block-b"));
                partial.breakpoints.place(
                    BreakpointLocation::SystemBlock(0),
                    CacheControl::Ephemeral5m,
                    "two_marker_pass",
                )?;
                partial.breakpoints.place(
                    BreakpointLocation::SystemBlock(1),
                    CacheControl::Ephemeral5m,
                    "two_marker_pass",
                )?;
                Ok(())
            }
        }

        // Start with a tracker pre-configured for budget=1 so the second
        // placement fails.
        let mut initial = PartialRequest::new("claude-opus-4-7");
        initial.breakpoints = BreakpointTracker::with_max(1);

        let passes: Vec<Box<dyn ComposerPass>> = vec![Box::new(TwoMarkerPass)];
        let err = compose(&passes, initial).expect_err("budget=1 must fail on second placement");

        match err {
            ProviderError::ComposerPassFailed { pass, source } => {
                assert_eq!(pass, "two_marker_pass");
                match *source {
                    ProviderError::CacheBreakpointBudgetExceeded {
                        budget,
                        attempted_by,
                        ..
                    } => {
                        assert_eq!(budget, 1);
                        assert_eq!(attempted_by, "two_marker_pass");
                    }
                    other => panic!("expected CacheBreakpointBudgetExceeded inside, got {other:?}"),
                }
            }
            other => panic!("expected ComposerPassFailed outer, got {other:?}"),
        }
    }

    #[test]
    fn finalize_empty_partial_produces_empty_request() {
        let p = PartialRequest::new("claude-opus-4-7");
        let out = finalize(p).expect("finalize succeeds");
        assert_eq!(out.model, "claude-opus-4-7");
        assert!(out.chat.system_blocks.is_none());
        assert!(out.chat.tools.is_none());
        assert!(out.chat.messages.is_empty());
    }

    #[test]
    fn finalize_populated_partial_round_trips_fields() {
        let mut p = PartialRequest::new("claude-opus-4-7");
        p.system_blocks.push(SystemBlock::new("sys-0"));
        p.messages.push(ChatMessage::user("msg-0"));

        let out = finalize(p).expect("finalize succeeds");
        assert_eq!(out.chat.system_blocks.as_ref().unwrap().len(), 1);
        assert_eq!(out.chat.messages.len(), 1);
    }

    // ---- Task 10 finalize tests ----

    // Helper: construct a partial with breakpoints placed on existing
    // blocks/messages, ready for finalize.
    fn partial_with_markers(
        sys_ttls: &[CacheControl],
        msg_ttls: &[CacheControl],
        pass_names: &[&'static str],
        include_beta: bool,
    ) -> PartialRequest {
        let mut p = PartialRequest::new("claude-opus-4-7");
        let mut name_idx = 0;

        for (i, ttl) in sys_ttls.iter().enumerate() {
            p.system_blocks.push(SystemBlock::new(format!("sys-{i}")));
            let name = pass_names.get(name_idx).copied().unwrap_or("test");
            p.breakpoints
                .place(BreakpointLocation::SystemBlock(i), ttl.clone(), name)
                .unwrap();
            name_idx += 1;
        }

        for (i, ttl) in msg_ttls.iter().enumerate() {
            p.messages.push(ChatMessage::user(format!("msg-{i}")));
            let name = pass_names.get(name_idx).copied().unwrap_or("test");
            p.breakpoints
                .place(BreakpointLocation::MessageBlock(i), ttl.clone(), name)
                .unwrap();
            name_idx += 1;
        }

        if include_beta {
            p.extra_headers.insert(
                "anthropic-beta".into(),
                "extended-cache-ttl-2025-04-11".into(),
            );
        }

        p
    }

    // ---- Marker application: system block cache_control set ----

    #[test]
    fn finalize_applies_markers_to_system_blocks() {
        let p = partial_with_markers(&[CacheControl::Ephemeral5m], &[], &["seg1"], false);
        let out = finalize(p).expect("finalize succeeds");
        let blocks = out.chat.system_blocks.unwrap();
        assert_eq!(blocks[0].cache_control, Some(CacheControl::Ephemeral5m));
    }

    // ---- Marker application: message cache_control set ----

    #[test]
    fn finalize_applies_markers_to_messages() {
        let p = partial_with_markers(&[], &[CacheControl::Ephemeral5m], &["seg2"], false);
        let out = finalize(p).expect("finalize succeeds");
        let cc = out.chat.messages[0]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref());
        assert_eq!(cc, Some(&CacheControl::Ephemeral5m));
    }

    // ---- Out-of-bounds system block index ----

    #[test]
    fn finalize_rejects_out_of_bounds_system_index() {
        let mut p = PartialRequest::new("claude-opus-4-7");
        // Place a marker at index 5 but don't add 6 system blocks.
        p.system_blocks.push(SystemBlock::new("only-one"));
        p.breakpoints
            .place(
                BreakpointLocation::SystemBlock(5),
                CacheControl::Ephemeral5m,
                "bad_pass",
            )
            .unwrap();

        let err = finalize(p).expect_err("out-of-bounds must fail");
        match err {
            ProviderError::InvalidBreakpointLocation { location, idx } => {
                assert_eq!(location, "system");
                assert_eq!(idx, 5);
            }
            other => panic!("expected InvalidBreakpointLocation, got {other:?}"),
        }
    }

    // ---- Out-of-bounds message index ----

    #[test]
    fn finalize_rejects_out_of_bounds_message_index() {
        let mut p = PartialRequest::new("claude-opus-4-7");
        p.breakpoints
            .place(
                BreakpointLocation::MessageBlock(0),
                CacheControl::Ephemeral5m,
                "bad_pass",
            )
            .unwrap();
        // No messages added.

        let err = finalize(p).expect_err("out-of-bounds must fail");
        match err {
            ProviderError::InvalidBreakpointLocation { location, idx } => {
                assert_eq!(location, "message");
                assert_eq!(idx, 0);
            }
            other => panic!("expected InvalidBreakpointLocation, got {other:?}"),
        }
    }

    // ---- ToolSchema rejected at finalize ----

    #[test]
    fn finalize_rejects_tool_schema_placement() {
        let mut p = PartialRequest::new("claude-opus-4-7");
        p.breakpoints
            .place(
                BreakpointLocation::ToolSchema(0),
                CacheControl::Ephemeral5m,
                "tool_pass",
            )
            .unwrap();

        let err = finalize(p).expect_err("ToolSchema must be rejected");
        match err {
            ProviderError::InvalidBreakpointLocation { location, idx } => {
                assert!(location.contains("tool"));
                assert_eq!(idx, 0);
            }
            other => panic!("expected InvalidBreakpointLocation, got {other:?}"),
        }
    }

    // ---- Extended-TTL no longer requires the beta header ----
    //
    // Anthropic dropped the `extended-cache-ttl-2025-04-11` beta as
    // a routing requirement in late 2025; current endpoints accept
    // `cache_control: { "ttl": "1h" }` directly. The old
    // "rejects without header" + "accepts with header" pair has been
    // replaced with a single test confirming extended TTL succeeds
    // WITHOUT the header. See
    // `docs/notes/2026-04-18-cache-ttl-research.md`.

    #[test]
    fn finalize_accepts_extended_ttl_without_beta_header() {
        let p = partial_with_markers(
            &[CacheControl::Ephemeral1h],
            &[],
            &["seg1"],
            false, // no beta header — no longer required
        );
        let out = finalize(p).expect("extended TTL without beta should succeed");
        let blocks = out.chat.system_blocks.unwrap();
        assert_eq!(blocks[0].cache_control, Some(CacheControl::Ephemeral1h));
    }

    /// Beta-header-present still accepted (back-compat: gateway may
    /// still emit the marker as a no-op).
    #[test]
    fn finalize_accepts_extended_ttl_with_beta_header() {
        let p = partial_with_markers(
            &[CacheControl::Ephemeral1h],
            &[CacheControl::Ephemeral1h],
            &["seg1", "seg2"],
            true,
        );
        let out = finalize(p).expect("finalize with beta should still succeed");
        let blocks = out.chat.system_blocks.unwrap();
        assert_eq!(blocks[0].cache_control, Some(CacheControl::Ephemeral1h));
    }

    // ---- TTL ordering: natural case succeeds (1h before 5m) ----

    #[test]
    fn finalize_accepts_natural_ttl_ordering() {
        let p = partial_with_markers(
            &[CacheControl::Ephemeral1h],
            &[CacheControl::Ephemeral5m, CacheControl::Ephemeral5m],
            &["seg1", "seg2", "seg3"],
            true,
        );
        finalize(p).expect("1h before 5m is natural ordering");
    }

    // ---- TTL ordering: violation detected (5m before 1h) ----

    #[test]
    fn finalize_rejects_reversed_ttl_ordering() {
        // Place 5m on a system block, then 1h on a message — reversed.
        let p = partial_with_markers(
            &[CacheControl::Ephemeral5m],
            &[CacheControl::Ephemeral1h],
            &["short_pass", "long_pass"],
            true,
        );
        let err = finalize(p).expect_err("reversed TTL ordering must fail");
        match err {
            ProviderError::TtlOrderingViolated {
                short_ttl_pass,
                long_ttl_pass,
            } => {
                assert_eq!(short_ttl_pass, "short_pass");
                assert_eq!(long_ttl_pass, "long_pass");
            }
            other => panic!("expected TtlOrderingViolated, got {other:?}"),
        }
    }

    // ---- TTL ordering: all-5m is fine ----

    #[test]
    fn finalize_accepts_all_5m_ttls() {
        let p = partial_with_markers(
            &[CacheControl::Ephemeral5m],
            &[CacheControl::Ephemeral5m, CacheControl::Ephemeral5m],
            &["seg1", "seg2", "seg3"],
            false,
        );
        finalize(p).expect("all 5m is fine");
    }

    // ---- Happy path: realistic 3-segment pipeline ----

    #[test]
    fn finalize_happy_path_realistic_3_segment() {
        let p = partial_with_markers(
            &[CacheControl::Ephemeral1h],
            &[CacheControl::Ephemeral5m, CacheControl::Ephemeral5m],
            &["segment_1", "segment_2", "segment_3"],
            true,
        );
        let out = finalize(p).expect("happy path succeeds");

        // Verify markers applied.
        let blocks = out.chat.system_blocks.unwrap();
        assert_eq!(blocks[0].cache_control, Some(CacheControl::Ephemeral1h));

        let msg0_cc = out.chat.messages[0]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref());
        let msg1_cc = out.chat.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref());
        assert_eq!(msg0_cc, Some(&CacheControl::Ephemeral5m));
        assert_eq!(msg1_cc, Some(&CacheControl::Ephemeral5m));
    }

    // ---- Belt-and-suspenders budget check at finalize ----

    #[test]
    fn finalize_budget_recheck_with_custom_tracker() {
        // Construct a partial with a tracker that has max=5 (so 5
        // placements are allowed at place-time) but finalize enforces
        // the ANTHROPIC_MAX_BREAKPOINTS=4 limit.
        let mut p = PartialRequest::new("claude-opus-4-7");
        p.breakpoints = BreakpointTracker::with_max(5);
        for i in 0..5 {
            p.system_blocks.push(SystemBlock::new(format!("sys-{i}")));
            p.breakpoints
                .place(
                    BreakpointLocation::SystemBlock(i),
                    CacheControl::Ephemeral5m,
                    "test_pass",
                )
                .unwrap();
        }

        let err = finalize(p).expect_err("5 markers must exceed budget at finalize");
        match err {
            ProviderError::CacheBreakpointBudgetExceeded {
                budget,
                attempted_by,
                ..
            } => {
                assert_eq!(budget, 4);
                assert_eq!(attempted_by, "finalize");
            }
            other => panic!("expected CacheBreakpointBudgetExceeded, got {other:?}"),
        }
    }

    // ---- Unsupported-binary stripping ---------------------------------

    fn binary_part(content_type: &str) -> ContentPart {
        use genai::chat::{Binary, BinarySource};
        use std::sync::Arc;
        ContentPart::Binary(Binary {
            content_type: content_type.to_string(),
            source: BinarySource::Base64(Arc::from("BASE64DATA")),
            name: Some(format!("test.{content_type}")),
        })
    }

    #[test]
    fn strip_drops_unsupported_top_level_binary_part() {
        use genai::chat::{ChatMessage, MessageContent};

        let mut msgs = vec![ChatMessage::user(MessageContent::from_parts(vec![
            ContentPart::Text("hello".to_string()),
            binary_part("image/svg+xml"),
            binary_part("image/png"),
        ]))];
        strip_unsupported_binary_parts(&mut msgs);

        let parts = msgs[0].content.parts();
        assert_eq!(parts.len(), 2, "svg should be stripped, text + png remain");
        assert!(matches!(parts[0], ContentPart::Text(_)));
        match &parts[1] {
            ContentPart::Binary(b) => assert_eq!(b.content_type, "image/png"),
            other => panic!("expected png binary, got {other:?}"),
        }
    }

    #[test]
    fn strip_drops_unsupported_binary_in_nested_tool_response() {
        use genai::chat::{ChatMessage, MessageContent, ToolResponse};

        let tr = ToolResponse::from_parts(
            "call_x",
            vec![
                ContentPart::Text("marker text".to_string()),
                binary_part("image/svg+xml"),
                binary_part("image/jpeg"),
            ],
        );
        let mut msgs = vec![ChatMessage::tool(tr)];
        strip_unsupported_binary_parts(&mut msgs);

        let parts = msgs[0].content.parts();
        assert_eq!(parts.len(), 1, "still one ToolResponse part");
        let ContentPart::ToolResponse(tr) = &parts[0] else {
            panic!("expected ToolResponse, got {:?}", parts[0]);
        };
        assert_eq!(tr.content.len(), 2, "svg stripped from nested content");
        match &tr.content[1] {
            ContentPart::Binary(b) => assert_eq!(b.content_type, "image/jpeg"),
            other => panic!("expected jpeg binary, got {other:?}"),
        }
    }

    #[test]
    fn strip_keeps_pdf_documents() {
        use genai::chat::{ChatMessage, MessageContent};

        let mut msgs = vec![ChatMessage::user(MessageContent::from_parts(vec![
            binary_part("application/pdf"),
        ]))];
        strip_unsupported_binary_parts(&mut msgs);
        assert_eq!(msgs[0].content.parts().len(), 1, "pdf must pass through");
    }

    #[test]
    fn strip_drops_unknown_application_octet_stream() {
        use genai::chat::{ChatMessage, MessageContent};

        let mut msgs = vec![ChatMessage::user(MessageContent::from_parts(vec![
            ContentPart::Text("doc".to_string()),
            binary_part("application/octet-stream"),
        ]))];
        strip_unsupported_binary_parts(&mut msgs);
        let parts = msgs[0].content.parts();
        assert_eq!(parts.len(), 1, "octet-stream is not supported by Anthropic");
        assert!(matches!(parts[0], ContentPart::Text(_)));
    }
}
