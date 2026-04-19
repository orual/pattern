//! Concrete composer-pass implementations for the three-segment cache layout.
//!
//! Each pass is a [`super::ComposerPass`] that appends content to a
//! [`super::PartialRequest`] and places one cache-breakpoint marker. The
//! canonical execution order is:
//!
//! 1. [`segment_1::Segment1Pass`] — system prompt + tool schemas.
//! 2. [`segment_2::Segment2Pass`] — prior-turn history + summary-head +
//!    memory-change pseudo-messages.
//! 3. [`segment_3::Segment3Pass`] — `[memory:current_state]` pseudo-turn.
//!
//! After all three passes, the caller appends fresh user input to
//! `partial.messages` (uncached), then calls [`super::finalize`] to apply
//! breakpoint markers and assemble the final
//! [`pattern_core::types::provider::CompletionRequest`].
//!
//! # Ordering matters
//!
//! The cache-breakpoint indices are positional — a pass that records
//! `BreakpointLocation::MessageBlock(5)` expects index 5 to remain stable.
//! Running passes out of order will misplace markers. The canonical order
//! above is enforced by convention (and documented here) rather than by
//! type-level sequencing; tests verify the combined pipeline produces the
//! correct marker count and placement.

pub mod segment_1;
pub mod segment_2;
pub mod segment_3;

pub use segment_1::Segment1Pass;
pub use segment_2::{Segment2Pass, synthesize_summary_message};
pub use segment_3::Segment3Pass;

#[cfg(test)]
mod tests {
    use genai::chat::{ChatMessage, SystemBlock};
    use jiff::Timestamp;
    use smol_str::SmolStr;

    use pattern_core::memory::{BlockMetadata, BlockSchema, BlockType, StructuredDocument};
    use pattern_core::types::block::{BlockWrite, BlockWriteKind};
    use pattern_core::types::origin::{Author, SystemReason};

    use crate::compose::PartialRequest;
    use crate::compose::breakpoints::BreakpointLocation;
    use crate::compose::pipeline::{ComposerPass, compose};
    use crate::compose::profile::CacheProfile;

    use super::*;

    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    fn make_doc(label: &str, content: &str) -> StructuredDocument {
        let mut metadata = BlockMetadata::standalone(BlockSchema::text());
        metadata.label = label.to_string();
        metadata.block_type = BlockType::Working;
        let doc = StructuredDocument::new_with_metadata(metadata, None);
        doc.set_text(content, true).unwrap();
        doc
    }

    fn make_block_write(handle: &str) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test"),
            block_type: BlockType::Working,
            rendered_content: "updated content".to_string(),
            kind: BlockWriteKind::Updated,
            previous_content_hash: None,
            previous_rendered_content: Some("old content".to_string()),
            at: Timestamp::from_second(1_745_000_000).unwrap(),
            author: Author::System {
                reason: SystemReason::ToolCall,
            },
        }
    }

    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    /// Create a partial with the extended-cache-ttl beta header set,
    /// required when using the default profile (which uses Ephemeral1h
    /// for segment 1).
    fn partial_with_beta(model: &str) -> PartialRequest {
        let mut p = PartialRequest::new(model);
        p.extra_headers.insert(
            "anthropic-beta".into(),
            "extended-cache-ttl-2025-04-11".into(),
        );
        p
    }

    // ---- AC7.1: exactly 3 cache markers after all three passes ----

    #[test]
    fn three_passes_produce_exactly_3_markers() {
        let profile = test_profile();
        let system_blocks = vec![
            SystemBlock::new("routing token"),
            SystemBlock::new("base instructions"),
            SystemBlock::new("persona"),
        ];
        let prior_msgs = vec![
            (SmolStr::new("msg-1"), ChatMessage::user("hello")),
            (SmolStr::new("msg-2"), ChatMessage::assistant("hi there")),
        ];
        let writes = vec![make_block_write("tasks")];
        let blocks = vec![make_doc("persona", "I am Sage.")];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(system_blocks, vec![], profile.clone())),
            Box::new(Segment2Pass::new(
                vec![],
                prior_msgs,
                &writes,
                profile.clone(),
            )),
            Box::new(Segment3Pass::new(blocks, profile)),
        ];

        let partial = partial_with_beta("claude-opus-4-7");
        let output = compose(&passes, partial).expect("compose succeeds");

        // After finalize expansion (Task 10), markers are now applied.
        // Verify compose succeeds and the output has markers applied.
        assert!(output.request.chat.system_blocks.is_some());
        assert!(!output.request.chat.messages.is_empty());

        // Count applied markers on system blocks + messages.
        let sys_markers = output
            .request
            .chat
            .system_blocks
            .as_ref()
            .map(|bs| bs.iter().filter(|b| b.cache_control.is_some()).count())
            .unwrap_or(0);
        let msg_markers = output
            .request
            .chat
            .messages
            .iter()
            .filter(|m| {
                m.options
                    .as_ref()
                    .and_then(|o| o.cache_control.as_ref())
                    .is_some()
            })
            .count();
        assert_eq!(
            sys_markers + msg_markers,
            3,
            "exactly 3 cache markers expected (1 sys + 2 msg)"
        );
    }

    // ---- AC7.1 via breakpoints: exactly 3 placements ----

    #[test]
    fn three_passes_place_exactly_3_breakpoints() {
        let profile = test_profile();
        let system_blocks = vec![SystemBlock::new("sys")];
        let prior_msgs = vec![(SmolStr::new("msg-1"), ChatMessage::user("hello"))];
        let blocks = vec![make_doc("persona", "content")];

        let seg1 = Segment1Pass::new(system_blocks, vec![], profile.clone());
        let seg2 = Segment2Pass::new(vec![], prior_msgs, &[], profile.clone());
        let seg3 = Segment3Pass::new(blocks, profile);

        let mut partial = PartialRequest::new("claude-opus-4-7");
        seg1.apply(&mut partial).unwrap();
        seg2.apply(&mut partial).unwrap();
        seg3.apply(&mut partial).unwrap();

        assert_eq!(
            partial.breakpoints.count(),
            3,
            "exactly 3 breakpoints expected"
        );

        // Verify marker locations: 1 system, 2 message.
        let placements = partial.breakpoints.placements();
        assert!(matches!(
            placements[0].location,
            BreakpointLocation::SystemBlock(_)
        ));
        assert!(matches!(
            placements[1].location,
            BreakpointLocation::MessageBlock(_)
        ));
        assert!(matches!(
            placements[2].location,
            BreakpointLocation::MessageBlock(_)
        ));
    }

    // ---- AC7.3: segment 3 [memory:current_state] present in pipeline ----

    #[test]
    fn pipeline_contains_current_state_in_segment_3() {
        let profile = test_profile();
        let blocks = vec![make_doc("tasks", "- review PR")];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(
                vec![SystemBlock::new("sys")],
                vec![],
                profile.clone(),
            )),
            Box::new(Segment2Pass::new(
                vec![],
                vec![(SmolStr::new("msg-1"), ChatMessage::user("hello"))],
                &[],
                profile.clone(),
            )),
            Box::new(Segment3Pass::new(blocks, profile)),
        ];

        let output =
            compose(&passes, partial_with_beta("claude-opus-4-7")).expect("compose succeeds");

        // The last message should be the current_state pseudo-turn.
        let last = output
            .request
            .chat
            .messages
            .last()
            .expect("messages not empty");
        let text = msg_text(last);
        assert!(
            text.contains("[memory:current_state]"),
            "last message must contain [memory:current_state]: {text}"
        );
    }

    // ---- AC8.3: [memory:updated] appears in segment 2 ----

    #[test]
    fn pipeline_contains_updated_pseudo_message_in_segment_2() {
        let profile = test_profile();
        let writes = vec![make_block_write("task_list")];
        let prior = vec![(SmolStr::new("msg-1"), ChatMessage::user("msg"))];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(
                vec![SystemBlock::new("sys")],
                vec![],
                profile.clone(),
            )),
            Box::new(Segment2Pass::new(vec![], prior, &writes, profile.clone())),
            Box::new(Segment3Pass::new(vec![], profile)),
        ];

        let output =
            compose(&passes, partial_with_beta("claude-opus-4-7")).expect("compose succeeds");

        // Find a message containing [memory:updated] — should be in
        // the segment 2 region (before the current_state message).
        let found = output
            .request
            .chat
            .messages
            .iter()
            .any(|m| msg_text(m).contains("[memory:updated]"));
        assert!(found, "must contain a [memory:updated] pseudo-message");
    }
}
