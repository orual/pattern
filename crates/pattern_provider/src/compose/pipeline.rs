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
//! # Phase 5 Task 3 scope
//!
//! This module ships the trait + orchestrator + a minimal
//! [`finalize`] that assembles a `CompletionRequest` without applying
//! breakpoint markers or running validation. Phase 5 Task 10 expands
//! `finalize` to:
//!
//! - Apply each placement's `control` to its indexed block/message/tool
//! - Validate breakpoint count ≤ 4 (belt-and-suspenders with the
//!   tracker's placement-time budget check)
//! - Validate each index is in-bounds for its collection
//! - Validate the required `extended-cache-ttl-2025-04-11` beta header
//!   is present when any placement uses an extended-TTL variant
//!
//! Task 3's minimal finalize lets Tasks 4-9 compose end-to-end and
//! inspect the assembled partial even before Task 10's validation
//! layer exists — useful for per-pass integration tests.

use pattern_core::error::ProviderError;
use pattern_core::types::provider::CompletionRequest;

use super::partial_request::PartialRequest;

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
pub fn compose(
    passes: &[Box<dyn ComposerPass>],
    initial: PartialRequest,
) -> Result<CompletionRequest, ProviderError> {
    let mut partial = initial;
    for pass in passes {
        pass.apply(&mut partial)
            .map_err(|source| ProviderError::ComposerPassFailed {
                pass: pass.name().to_string(),
                source: Box::new(source),
            })?;
    }
    finalize(partial)
}

/// Assemble a completed [`PartialRequest`] into a [`CompletionRequest`].
///
/// Phase 5 Task 3 provides a minimal pass-through: collects the
/// accumulated fields into a [`genai::chat::ChatRequest`] without
/// applying `cache_control` markers or running validation. See module
/// docs for the Task 10 expansion plan.
pub fn finalize(partial: PartialRequest) -> Result<CompletionRequest, ProviderError> {
    let PartialRequest {
        model,
        system_blocks,
        messages,
        tools,
        options,
        extra_headers: _, // Phase 5 Task 10: merge into outbound header set.
        breakpoints: _,   // Phase 5 Task 10: walk + apply + validate.
    } = partial;

    let mut chat = genai::chat::ChatRequest::new(messages);
    // Always use per-block `system_blocks` (never the legacy single-
    // string `system` field) so cache_control markers can be attached
    // per-block at Task 10's finalize expansion.
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
    })
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
        let out =
            compose(&passes, PartialRequest::new("claude-opus-4-7")).expect("compose succeeds");

        let blocks = out.chat.system_blocks.as_ref().expect("blocks populated");
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
}
