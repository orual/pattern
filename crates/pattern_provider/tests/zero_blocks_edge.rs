// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! AC7.6 regression: composer emits segment 3 pseudo-turn even with zero
//! loaded blocks. The `[memory:current_state]` body becomes "(no blocks
//! loaded)" and the segment-3 cache_control marker still lands on the
//! pseudo-turn message. Later, loading a block changes the body without
//! changing the marker shape.

use genai::chat::{CacheControl, SystemBlock};
use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::{BlockMetadata, BlockSchema, MemoryBlockType};
use pattern_provider::compose::{
    CacheProfile, ComposerPass, PartialRequest,
    passes::{Segment1Pass, Segment2Pass, Segment3Pass},
    pipeline::compose,
};

// ---- Test helpers -----------------------------------------------------------

fn system_blocks() -> Vec<SystemBlock> {
    vec![
        SystemBlock::new("You are Pattern."),
        SystemBlock::new("Base instructions."),
    ]
}

/// Build a `(CacheProfile, PartialRequest)` pair for the default subscriber
/// profile. The request has the extended-cache-ttl beta header pre-set
/// because the all-1h default profile requires it at finalize time.
fn profile_with_beta() -> (CacheProfile, PartialRequest) {
    let profile = CacheProfile::default_anthropic_subscriber();
    let mut partial = PartialRequest::new("claude-opus-4-7");
    // Default profile is all-1h; finalize validates the extended-TTL beta.
    partial.extra_headers.insert(
        "anthropic-beta".into(),
        "extended-cache-ttl-2025-04-11".into(),
    );
    (profile, partial)
}

/// Construct a `StructuredDocument` suitable for use in tests.
///
/// Mirrors the `make_doc` helper in
/// `crates/pattern_provider/src/compose/passes/segment_3.rs::tests` and
/// `crates/pattern_provider/src/compose/passes.rs::tests`. Duplicated here
/// because those helpers live inside `#[cfg(test)] mod tests {}` blocks and
/// are not accessible across the crate boundary.
fn make_doc(label: &str, content: &str) -> StructuredDocument {
    let mut metadata = BlockMetadata::standalone(BlockSchema::text());
    metadata.label = label.to_string();
    metadata.block_type = MemoryBlockType::Working;
    let doc = StructuredDocument::new_with_metadata(metadata, None);
    doc.set_text(content, true).unwrap();
    doc
}

// ---- AC7.6: zero blocks still emits one segment-3 message ------------------

/// The composer must produce exactly one message (the segment-3 pseudo-turn)
/// even when no memory blocks are loaded. Segment 2 is empty (no prior
/// messages, no summaries, no writes), so the single message comes from
/// segment 3 alone.
#[test]
fn zero_blocks_emits_present_but_empty_segment_3() {
    let (profile, initial) = profile_with_beta();

    let passes: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(system_blocks(), vec![], profile.clone())),
        Box::new(Segment2Pass::new(vec![], vec![], profile.clone())),
        Box::new(Segment3Pass::new(vec![], profile)),
    ];

    let output = compose(&passes, initial).expect("compose succeeds with zero blocks");

    // Segment 2 pushed nothing (empty summary_head + prior + pseudo).
    // Segment 3 pushes exactly one message (the pseudo-turn).
    let messages = &output.request.chat.messages;
    assert_eq!(
        messages.len(),
        1,
        "segment 3 should emit exactly one message even with zero blocks"
    );

    // The message body must contain the empty-state markers.
    let text = messages[0].content.joined_texts().unwrap_or_default();
    assert!(
        text.contains("[memory:current_state]"),
        "segment 3 message must contain [memory:current_state] tag; got: {text:?}"
    );
    assert!(
        text.contains("(no blocks loaded)"),
        "segment 3 message must contain '(no blocks loaded)' body; got: {text:?}"
    );
    assert!(
        text.contains("<system-reminder>"),
        "segment 3 message must be wrapped in <system-reminder>; got: {text:?}"
    );
}

// ---- AC7.6: zero blocks still places the segment-3 cache marker -------------

/// The segment-3 cache_control marker must be present even when no blocks are
/// loaded — the marker's existence is what preserves cache-boundary consistency
/// across turns.
#[test]
fn zero_blocks_still_places_segment_3_cache_marker() {
    let (profile, initial) = profile_with_beta();

    let passes: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(system_blocks(), vec![], profile.clone())),
        Box::new(Segment2Pass::new(vec![], vec![], profile.clone())),
        Box::new(Segment3Pass::new(vec![], profile)),
    ];

    let output = compose(&passes, initial).expect("compose succeeds");

    // Pluck the cache_control off the last (only) message.
    let last = output.request.chat.messages.last().unwrap();
    let cc = last
        .options
        .as_ref()
        .and_then(|o| o.cache_control.as_ref())
        .expect("segment 3 cache_control must be present even with zero blocks");
    assert!(
        matches!(cc, CacheControl::Ephemeral1h),
        "segment 3 cache_control should be Ephemeral1h per default profile; got: {cc:?}"
    );
}

// ---- Loading a block changes body, not marker shape -------------------------

/// Demonstrates that the transition from zero-blocks to one-block state changes
/// the pseudo-turn BODY but leaves the segment-3 MARKER SHAPE unchanged. Both
/// turns have a cache_control marker on the last message at the same TTL.
///
/// This is the key invariant for cache-boundary consistency: agents whose memory
/// state transitions from empty to populated don't see segment 3 structurally
/// shift; only the content inside the marker changes.
#[test]
fn loading_a_block_changes_segment_3_body_not_marker_shape() {
    let (profile_a, initial_a) = profile_with_beta();
    let (profile_b, initial_b) = profile_with_beta();

    // Turn A: zero blocks.
    let passes_a: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(
            system_blocks(),
            vec![],
            profile_a.clone(),
        )),
        Box::new(Segment2Pass::new(vec![], vec![], profile_a.clone())),
        Box::new(Segment3Pass::new(vec![], profile_a)),
    ];
    let output_a = compose(&passes_a, initial_a).expect("turn A composes");
    let req_a = output_a.request;

    // Turn B: one block loaded with a unique sentinel.
    let block = make_doc("scratch", "SENTINEL_CONTENT_FOR_TURN_B");
    let passes_b: Vec<Box<dyn ComposerPass>> = vec![
        Box::new(Segment1Pass::new(
            system_blocks(),
            vec![],
            profile_b.clone(),
        )),
        Box::new(Segment2Pass::new(vec![], vec![], profile_b.clone())),
        Box::new(Segment3Pass::new(vec![block], profile_b)),
    ];
    let output_b = compose(&passes_b, initial_b).expect("turn B composes");
    let req_b = output_b.request;

    // Body must differ between turns.
    let text_a = req_a
        .chat
        .messages
        .last()
        .unwrap()
        .content
        .joined_texts()
        .unwrap_or_default();
    let text_b = req_b
        .chat
        .messages
        .last()
        .unwrap()
        .content
        .joined_texts()
        .unwrap_or_default();

    assert!(
        text_a.contains("(no blocks loaded)"),
        "turn A must contain '(no blocks loaded)'; got: {text_a:?}"
    );
    assert!(
        text_b.contains("SENTINEL_CONTENT_FOR_TURN_B"),
        "turn B must contain the sentinel; got: {text_b:?}"
    );
    assert!(
        !text_b.contains("(no blocks loaded)"),
        "turn B must not contain '(no blocks loaded)'; got: {text_b:?}"
    );

    // Marker shape must be identical: same TTL, same position (last message).
    let cc_a = req_a
        .chat
        .messages
        .last()
        .and_then(|m| m.options.as_ref())
        .and_then(|o| o.cache_control.as_ref());
    let cc_b = req_b
        .chat
        .messages
        .last()
        .and_then(|m| m.options.as_ref())
        .and_then(|o| o.cache_control.as_ref());

    assert_eq!(
        format!("{cc_a:?}"),
        format!("{cc_b:?}"),
        "segment 3 marker shape should be identical between empty and non-empty block states"
    );
    // Both must be non-None.
    assert!(
        cc_a.is_some(),
        "turn A segment 3 cache_control must be present"
    );
    assert!(
        cc_b.is_some(),
        "turn B segment 3 cache_control must be present"
    );
}
