//! Regression test for AC7.2 (segment 1 contains no block content).
//!
//! # Audit summary (performed 2026-04-17)
//!
//! ## `rg '\[memory:'` outside `compose/`
//!
//! Running:
//! ```text
//! rg '\[memory:' crates/pattern_core/src/ crates/pattern_provider/src/ \
//!     crates/pattern_runtime/src/ | grep -v test | grep -v compose
//! ```
//!
//! Result: **only doc comments in `pattern_core/src/types/block.rs`**.
//! Specifically, the module-level doc for `block.rs` describes what the
//! pseudo-message emission renders (`[memory:written]`, `[memory:updated]`,
//! etc.) as documentation of the *format*. No production call site outside
//! `pattern_provider/src/compose/` actually emits or renders `[memory:*]`
//! markers into any request field.
//!
//! ## `render_for_context | StructuredDocument::render | get_rendered_content`
//!
//! Running:
//! ```text
//! rg 'render_for_context|StructuredDocument::render|get_rendered_content' \
//!     crates/pattern_core/src/ crates/pattern_provider/src/ \
//!     crates/pattern_runtime/src/ | grep -v test | grep -v compose | grep -v memory
//! ```
//!
//! Result: **only doc comments in `pattern_core/src/types/block.rs`**.
//! `StructuredDocument::render` is owned by `pattern_core::memory` (the
//! storage layer). The composer's `current_state` renderer is the only
//! consumer; it lives in `pattern_provider/src/compose/current_state.rs`
//! (matched by the `grep -v compose` exclusion being intentionally not
//! applied here — those hits *are* the expected single consumer).
//!
//! ## Conclusion
//!
//! Phase 2 staged `context/builder.rs` out of the workspace, removing the
//! last pre-v3 path that could mix block content into the system prompt.
//! The Phase 4 shaper and Phase 5 composer never re-introduced it. This test
//! pins the invariant with an integration test across the full compose
//! pipeline.
//!
//! The composer's `Segment1Pass` is the only pass that writes to
//! `system_blocks`; it receives pre-rendered identity/base-instructions/
//! persona text and has no access to block data. Memory-block content lives
//! exclusively in segment 3's `[memory:current_state]` pseudo-turn, placed by
//! `Segment3Pass`.

use genai::chat::{SystemBlock, Tool};
use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::{BlockMetadata, BlockSchema, BlockType};
use pattern_provider::compose::{
    BreakpointLocation, CacheProfile, ComposerPass, PartialRequest,
    passes::{Segment1Pass, Segment2Pass, Segment3Pass},
};

// ---- Test helpers -----------------------------------------------------------

/// Unique sentinel strings that must appear in segment 3 but must NOT
/// appear anywhere in system_blocks (segment 1).
const SENTINEL_LABEL_A: &str = "AUDIT_SENTINEL_BLOCK_LABEL_ALPHA_7F2E9C";
const SENTINEL_CONTENT_A: &str =
    "AUDIT_SENTINEL_BLOCK_CONTENT_ALPHA_7F2E9C: tasks and details here";
const SENTINEL_LABEL_B: &str = "AUDIT_SENTINEL_BLOCK_LABEL_BETA_3D8A1F";
const SENTINEL_CONTENT_B: &str = "AUDIT_SENTINEL_BLOCK_CONTENT_BETA_3D8A1F: identity and context";

/// Construct a `StructuredDocument` from metadata + content without a
/// database backing. This matches the pattern used in unit tests in
/// `compose/passes/segment_3.rs` and `compose/passes.rs`.
fn make_doc(label: &str, content: &str) -> StructuredDocument {
    let mut metadata = BlockMetadata::standalone(BlockSchema::text());
    metadata.label = label.to_string();
    metadata.block_type = BlockType::Working;
    let doc = StructuredDocument::new_with_metadata(metadata, None);
    doc.set_text(content, true)
        .expect("set_text on a fresh StructuredDocument must succeed");
    doc
}

/// Construct the system blocks for segment 1: the routing token (slot 0)
/// plus base-instructions block (slot 1). Neither slot contains block content.
fn build_system_blocks() -> Vec<SystemBlock> {
    vec![
        SystemBlock::new("You are Claude Code, Anthropic's official CLI."),
        SystemBlock::new(format!(
            "You are NOT Claude Code.\n{}",
            pattern_core::DEFAULT_BASE_INSTRUCTIONS
        )),
    ]
}

/// Tool list — empty is fine for this audit; we only need to prove block
/// content doesn't leak from segment 3 into system_blocks.
fn build_tools() -> Vec<Tool> {
    vec![]
}

/// Two memory blocks with distinct sentinel labels and content. The
/// sentinel strings must appear in the segment 3 output but must NOT
/// appear in any system_block.
fn build_sentinel_blocks() -> Vec<StructuredDocument> {
    vec![
        make_doc(SENTINEL_LABEL_A, SENTINEL_CONTENT_A),
        make_doc(SENTINEL_LABEL_B, SENTINEL_CONTENT_B),
    ]
}

/// Build a `PartialRequest` with the `extended-cache-ttl-2025-04-11` beta
/// header required by the default profile (all-1h TTLs).
fn partial_with_beta(model: &str) -> PartialRequest {
    let mut p = PartialRequest::new(model);
    p.extra_headers.insert(
        "anthropic-beta".into(),
        "extended-cache-ttl-2025-04-11".into(),
    );
    p
}

// ---- Tests ------------------------------------------------------------------

/// AC7.2 — integration regression.
///
/// Compose a full three-segment request with non-empty sentinel blocks and
/// assert:
///
/// (a) No `[memory:*]` tag appears in any `system_block` (the cached
///     segment-1 region).
/// (b) No sentinel block label or content appears in any `system_block`.
/// (c) The sentinel content DOES appear in the segment-3 `[memory:current_state]`
///     pseudo-turn (the last message after all three passes).
/// (d) The last message contains the `[memory:current_state]` tag.
#[test]
fn segment_1_contains_no_memory_block_content_or_labels() {
    let profile = CacheProfile::default_anthropic_subscriber();
    let system_blocks = build_system_blocks();
    let tools = build_tools();
    let blocks = build_sentinel_blocks();

    let passes: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(
            system_blocks.clone(),
            tools,
            profile.clone(),
        )),
        // Segment 2: no prior messages, no block writes — clean slate.
        Box::new(Segment2Pass::new(vec![], vec![], &[], profile.clone())),
        Box::new(Segment3Pass::new(blocks, profile)),
    ];

    let initial = partial_with_beta("claude-opus-4-7");
    let output = pattern_provider::compose::compose(&passes, initial)
        .expect("compose with sentinel blocks must succeed");
    let req = output.request;

    // ---- (a + b) Segment 1 invariants: system_blocks contain no block data --

    let sys_blocks = req
        .chat
        .system_blocks
        .as_ref()
        .expect("system_blocks must be populated after Segment1Pass");
    assert!(
        !sys_blocks.is_empty(),
        "system_blocks must be non-empty after segment 1"
    );

    for (idx, block) in sys_blocks.iter().enumerate() {
        // (a) No [memory:*] tag of any kind.
        assert!(
            !block.text.contains("[memory:"),
            "system_blocks[{idx}] contains a [memory:*] tag — block content leaked into segment 1. \
             First 200 chars: {:?}",
            &block.text[..block.text.len().min(200)]
        );

        // (b) No sentinel labels.
        assert!(
            !block.text.contains(SENTINEL_LABEL_A),
            "system_blocks[{idx}] contains sentinel label A — block label leaked into segment 1"
        );
        assert!(
            !block.text.contains(SENTINEL_LABEL_B),
            "system_blocks[{idx}] contains sentinel label B — block label leaked into segment 1"
        );

        // (b) No sentinel content.
        assert!(
            !block.text.contains(SENTINEL_CONTENT_A),
            "system_blocks[{idx}] contains sentinel content A — block content leaked into segment 1"
        );
        assert!(
            !block.text.contains(SENTINEL_CONTENT_B),
            "system_blocks[{idx}] contains sentinel content B — block content leaked into segment 1"
        );
    }

    // ---- (c + d) Segment 3 positive assertions: current_state has block data --

    let messages = &req.chat.messages;
    assert!(
        !messages.is_empty(),
        "messages must be non-empty after Segment3Pass"
    );

    let last_msg = messages
        .last()
        .expect("at least one message from Segment3Pass");
    let last_text = last_msg.content.joined_texts().unwrap_or_default();

    // (d) Must have the [memory:current_state] wrapper tag.
    assert!(
        last_text.contains("[memory:current_state]"),
        "segment 3 (last message) must contain [memory:current_state]; got: {last_text:?}"
    );

    // (c) Both sentinels must appear in the current_state render.
    assert!(
        last_text.contains(SENTINEL_LABEL_A),
        "segment 3 must contain sentinel label A to confirm block data reaches segment 3; \
         got: {last_text:?}"
    );
    assert!(
        last_text.contains(SENTINEL_LABEL_B),
        "segment 3 must contain sentinel label B; got: {last_text:?}"
    );
}

/// Segment 1 marker is placed on a system block, never on a message.
///
/// Verifies the structural invariant that the cache boundary for the
/// system-prompt region stays in `SystemBlock` territory, not in the
/// message list (which would indicate block rendering had moved into the
/// system-prompt path).
#[test]
fn segment_1_cache_marker_is_on_system_block_not_message() {
    let profile = CacheProfile::default_anthropic_subscriber();
    let mut initial = partial_with_beta("claude-opus-4-7");

    let pass = Segment1Pass::new(build_system_blocks(), vec![], profile);
    pass.apply(&mut initial)
        .expect("Segment1Pass apply must succeed");

    let placements = initial.breakpoints.placements();
    assert_eq!(
        placements.len(),
        1,
        "exactly one breakpoint placed by Segment1Pass"
    );

    match placements[0].location {
        BreakpointLocation::SystemBlock(_) => { /* expected */ }
        ref other => panic!("segment-1 cache marker must land on a SystemBlock, got {other:?}"),
    }

    // Messages must be untouched by segment 1 (no block content injected).
    assert!(
        initial.messages.is_empty(),
        "Segment1Pass must not add any messages"
    );
}
